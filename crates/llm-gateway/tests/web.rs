//! The usage view end to end: the owner's login against a fake OIDC provider, and the page's
//! numbers from the usage database.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use jiff::Timestamp;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use llm_gateway::provider::ProviderFuture;
use llm_gateway::web::LoginSettings;
use llm_gateway::web::oidc;
use llm_gateway::{
    Callers, Eur, Gateway, Prices, Provider, RateLimit, Refusal, RetryPolicy, Spheres, Usage,
    UsageStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "llm-gateway-web";
const KEY_ID: &str = "web-test-key";
const OWNER_GROUP: &str = "llm_gateway_owner";

/// What the fake provider puts into the ID token for an authorization code.
#[derive(Clone)]
struct Grant {
    code_challenge: String,
    nonce: String,
    groups: Vec<String>,
}

#[derive(Clone)]
struct FakeIdp {
    issuer: String,
    signing: Arc<EncodingKey>,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}

/// Starts a fake OIDC provider.
async fn start_idp() -> FakeIdp {
    let seed: [u8; 32] = {
        let mut seed = [0u8; 32];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut seed);
        seed
    };
    let key = SigningKey::from_bytes(&seed);
    let jwks = json!({ "keys": [{
        "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": KEY_ID,
        "x": URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
    }]});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let idp = FakeIdp {
        issuer: format!("{base}/auth/v1/"),
        signing: Arc::new(EncodingKey::from_ed_der(
            key.to_pkcs8_der().unwrap().as_bytes(),
        )),
        grants: Arc::default(),
    };
    let discovery = json!({
        "issuer": idp.issuer,
        "jwks_uri": format!("{base}/jwks"),
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
    });
    let app = Router::new()
        .route(
            "/auth/v1/.well-known/openid-configuration",
            get(move || async move { Json(discovery) }),
        )
        .route("/jwks", get(move || async move { Json(jwks) }))
        .route("/token", post(token))
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    idp
}

/// The fake token endpoint: checks PKCE and returns a signed ID token.
async fn token(
    State(idp): State<FakeIdp>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Json<Value>, (axum::http::StatusCode, &'static str)> {
    let bad = |why| Err((axum::http::StatusCode::BAD_REQUEST, why));
    let Some(grant) = idp.grants.lock().unwrap().remove(&form["code"]) else {
        return bad("unknown code");
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
    if challenge != grant.code_challenge || form["client_id"] != CLIENT_ID {
        return bad("PKCE or client mismatch");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some(KEY_ID.into());
    header.typ = Some("JWT".into());
    let id_token = jsonwebtoken::encode(
        &header,
        &json!({
            "iss": idp.issuer, "aud": CLIENT_ID, "sub": "user-1",
            "email": "owner@example.org", "exp": now + 300, "iat": now,
            "nonce": grant.nonce, "groups": grant.groups,
        }),
        &idp.signing,
    )
    .unwrap();
    Ok(Json(
        json!({ "access_token": "unused", "token_type": "Bearer", "id_token": id_token }),
    ))
}

/// A provider that is never called: the view only reads the database.
struct NoProvider;

impl Provider for NoProvider {
    fn name(&self) -> &str {
        "mistral"
    }

    fn embed<'a>(&'a self, _model: &'a str, _inputs: &'a [String]) -> ProviderFuture<'a> {
        unreachable!("the usage view never calls the provider")
    }
}

fn at(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// A gateway with a 10 EUR budget and some usage, seen on 3 October 2026 at noon.
fn gateway() -> Arc<Gateway> {
    let store = UsageStore::in_memory().unwrap();
    for (when, caller, sphere, cost) in [
        ("2026-09-20T10:00:00Z", "mnemograph", "work", "1.5"),
        ("2026-10-01T08:00:00Z", "mnemograph", "work", "0.75"),
        ("2026-10-02T09:00:00Z", "documents", "private", "3.25"),
    ] {
        store
            .record_usage(&Usage {
                at: at(when),
                caller: caller.into(),
                sphere: sphere.into(),
                provider: "mistral".into(),
                model: "mistral-embed".into(),
                inputs: 3,
                input_tokens: 1200,
                cost: cost.parse().unwrap(),
            })
            .unwrap();
    }
    store
        .record_refusal(&Refusal {
            at: at("2026-10-02T10:00:00Z"),
            caller: "documents".into(),
            sphere: "secret".into(),
            model: "mistral-embed".into(),
            reason: "sphere_rule".into(),
        })
        .unwrap();
    Arc::new(Gateway {
        callers: Callers::default(),
        prices: Prices::default(),
        budget: "10".parse::<Eur>().unwrap(),
        spheres: Spheres::default(),
        rate_limit: RateLimit::new(1000),
        retry: RetryPolicy::default(),
        provider: Arc::new(NoProvider),
        store,
        clock: Arc::new(|| at("2026-10-03T12:00:00Z")),
    })
}

struct Ui {
    base: String,
    http: reqwest::Client,
    idp: Option<FakeIdp>,
}

impl Ui {
    /// The usage view with the owner's login against a fake provider.
    async fn start() -> Self {
        let idp = start_idp().await;
        let provider = oidc::Provider::discover(&idp.issuer).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let login = LoginSettings {
            provider: Arc::new(provider),
            client_id: CLIENT_ID.into(),
            client_secret: None,
            redirect_uri: format!("{base}/auth/callback"),
            owner_group: OWNER_GROUP.into(),
            secure_cookies: false,
        };
        Self::serve(listener, base, Some(login), Some(idp))
    }

    /// The usage view without a login, as in development.
    async fn start_without_login() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        Self::serve(listener, base, None, None)
    }

    fn serve(
        listener: tokio::net::TcpListener,
        base: String,
        login: Option<LoginSettings>,
        idp: Option<FakeIdp>,
    ) -> Self {
        let app = llm_gateway::web::router(gateway(), login);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            idp,
        }
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut request = self.http.get(format!("{}{path}", self.base));
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.send().await.unwrap()
    }

    /// Starts a login; returns the login cookie and the authorization request's parameters.
    async fn start_login(&self) -> (String, HashMap<String, String>) {
        let response = self.get("/login?next=%2F%3Fmonth%3D2026-10", None).await;
        assert_eq!(response.status(), 303);
        let cookie = set_cookie(&response, "llm_gateway_login").expect("login cookie");
        let location = reqwest::Url::parse(location(&response)).unwrap();
        assert!(location.path().ends_with("/authorize"), "{location}");
        (cookie, location.query_pairs().into_owned().collect())
    }

    /// Lets the fake provider accept the login for `params` with these groups and nonce.
    fn grant(&self, params: &HashMap<String, String>, groups: &[&str], nonce: Option<&str>) {
        self.idp.as_ref().unwrap().grants.lock().unwrap().insert(
            "code-1".into(),
            Grant {
                code_challenge: params["code_challenge"].clone(),
                nonce: nonce.unwrap_or(&params["nonce"]).into(),
                groups: groups.iter().map(|g| g.to_string()).collect(),
            },
        );
    }

    async fn callback(&self, state: &str, cookie: Option<&str>) -> reqwest::Response {
        self.get(&format!("/auth/callback?code=code-1&state={state}"), cookie)
            .await
    }

    /// Logs in as a member of the owner group and returns the session cookie.
    async fn log_in(&self) -> String {
        let (login_cookie, params) = self.start_login().await;
        self.grant(&params, &[OWNER_GROUP], None);
        let response = self.callback(&params["state"], Some(&login_cookie)).await;
        assert_eq!(response.status(), 303);
        assert_eq!(location(&response), "/?month=2026-10");
        set_cookie(&response, "llm_gateway_session").expect("session cookie")
    }
}

fn location(response: &reqwest::Response) -> &str {
    response.headers()["location"].to_str().unwrap()
}

/// The `name=value` pair of a Set-Cookie header, if the response sets that cookie.
fn set_cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap().to_owned())
        .find(|pair| pair.starts_with(&format!("{name}=")) && pair.len() > name.len() + 1)
}

#[tokio::test]
async fn no_access_without_login() {
    let ui = Ui::start().await;
    for path in ["/", "/?month=2026-09"] {
        let response = ui.get(path, None).await;
        assert_eq!(response.status(), 303, "{path}");
        assert!(location(&response).starts_with("/login?next=%2F"), "{path}");
    }
    let response = ui.get("/", Some("llm_gateway_session=forged")).await;
    assert_eq!(response.status(), 303);
}

#[tokio::test]
async fn the_owner_logs_in_sees_the_usage_and_logs_out() {
    let ui = Ui::start().await;
    let session = ui.log_in().await;

    let response = ui.get("/?month=2026-10", Some(&session)).await;
    assert_eq!(response.status(), 200);
    let html = response.text().await.unwrap();
    assert!(html.contains("owner@example.org"), "{html}");
    assert!(html.contains("October 2026"));

    let response = ui
        .http
        .post(format!("{}/logout", ui.base))
        .header("cookie", &session)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(location(&response), "/logged-out");
    assert_eq!(ui.get("/", Some(&session)).await.status(), 303);
}

#[tokio::test]
async fn only_the_owner_group_may_log_in() {
    let ui = Ui::start().await;
    let (login_cookie, params) = ui.start_login().await;
    ui.grant(&params, &["someone_else"], None);
    let response = ui.callback(&params["state"], Some(&login_cookie)).await;
    assert_eq!(response.status(), 403);
    assert!(set_cookie(&response, "llm_gateway_session").is_none());
}

#[tokio::test]
async fn a_login_must_come_back_to_the_browser_that_started_it() {
    let ui = Ui::start().await;
    let (_, params) = ui.start_login().await;
    ui.grant(&params, &[OWNER_GROUP], None);
    let response = ui.callback(&params["state"], None).await;
    assert_eq!(response.status(), 400);
    assert!(set_cookie(&response, "llm_gateway_session").is_none());
}

#[tokio::test]
async fn an_id_token_of_another_login_is_refused() {
    let ui = Ui::start().await;
    let (login_cookie, params) = ui.start_login().await;
    ui.grant(&params, &[OWNER_GROUP], Some("another-nonce"));
    let response = ui.callback(&params["state"], Some(&login_cookie)).await;
    assert_eq!(response.status(), 502);
    assert!(set_cookie(&response, "llm_gateway_session").is_none());
}

/// The text of the page without tags, with whitespace collapsed.
fn text(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tokio::test]
async fn the_page_shows_budget_usage_and_refusals() {
    let ui = Ui::start_without_login().await;
    let response = ui.get("/", None).await;
    assert_eq!(response.status(), 200);
    let html = response.text().await.unwrap();
    let page = text(&html);

    // October: 0.75 + 3.25 of 10 EUR.
    assert!(
        page.contains("40 % of the monthly budget of 10.00 € used so far"),
        "{page}"
    );
    assert!(page.contains("spent 4.00 €"), "{page}");
    assert!(page.contains("left 6.00 €"), "{page}");
    assert!(
        page.contains("calls 2 texts 6 tokens 2 400 refusals 1"),
        "{page}"
    );
    // Per caller, most expensive first; per sphere; per model.
    assert!(
        page.contains("documents 1 3 1 200 3.25 € mnemograph 1 3 1 200 0.7500 €"),
        "{page}"
    );
    assert!(
        page.contains("private 1 3 1 200 3.25 € work 1 3 1 200 0.7500 €"),
        "{page}"
    );
    assert!(page.contains("mistral-embed 2 6 2 400 4.00 €"), "{page}");
    // The chart has one line per caller over the three days so far.
    assert_eq!(html.matches("<polyline").count(), 2);
    assert!(page.contains("1 Oct 3 Oct"), "{page}");
    // Refusals and the earlier month.
    assert!(
        page.contains("sphere rule documents secret 1 2026-10-02 10:00 UTC"),
        "{page}"
    );
    assert!(html.contains("href=\"/?month=2026-09\">September 2026</a>"));
    assert!(page.contains("September 2026 1 1 200 1.50 € 0"), "{page}");
    // The current month has no later one.
    assert!(!html.contains("rel=\"next\""));
}

#[tokio::test]
async fn earlier_months_can_be_shown() {
    let ui = Ui::start_without_login().await;
    let html = ui.get("/?month=2026-09", None).await.text().await.unwrap();
    let page = text(&html);
    assert!(page.contains("September 2026"));
    assert!(
        page.contains("15 % of the monthly budget of 10.00 € used."),
        "{page}"
    );
    assert!(page.contains("No requests were refused this month."));
    assert!(html.contains("href=\"/?month=2026-10\" rel=\"next\""));

    assert_eq!(ui.get("/?month=2026-11", None).await.status(), 404);
    assert_eq!(ui.get("/?month=october", None).await.status(), 400);
}
