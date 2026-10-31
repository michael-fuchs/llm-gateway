//! The embeddings API against a fake provider: authentication, usage recording and the
//! monthly budget.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use jiff::Timestamp;
use llm_gateway::provider::ProviderFuture;
use llm_gateway::{
    Callers, Embeddings, Eur, Gateway, Prices, Provider, ProviderError, Refusal, Usage, UsageStore,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const MNEMOGRAPH_KEY: &str = "mnemograph-key-0123456789";
const DOCUMENTS_KEY: &str = "documents-key-0123456789";

/// Embeds each text as `[length]` and counts ten tokens per text, or fails when told to.
#[derive(Default)]
struct FakeProvider {
    calls: AtomicUsize,
    fail: Mutex<bool>,
}

impl Provider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }

    fn embed<'a>(&'a self, _model: &'a str, inputs: &'a [String]) -> ProviderFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let fail = *self.fail.lock().unwrap();
        Box::pin(async move {
            if fail {
                return Err(ProviderError("HTTP 503 from the fake provider".into()));
            }
            Ok(Embeddings {
                vectors: inputs.iter().map(|t| vec![t.len() as f32]).collect(),
                input_tokens: 10 * inputs.len() as u64,
            })
        })
    }
}

struct Harness {
    gateway: Arc<Gateway>,
    provider: Arc<FakeProvider>,
    now: Arc<Mutex<Timestamp>>,
}

/// A gateway with two callers, the models `mistral-embed` (0.1 EUR per million tokens) and
/// `pricey` (100,000 EUR per million tokens, so 1 EUR per text), and the given budget.
fn harness(budget: &str) -> Harness {
    let provider = Arc::new(FakeProvider::default());
    let now = Arc::new(Mutex::new(at("2026-10-31T12:00:00Z")));
    let clock_now = now.clone();
    let gateway = Arc::new(Gateway {
        callers: Callers::parse(&[
            format!("mnemograph={MNEMOGRAPH_KEY}"),
            format!("documents={DOCUMENTS_KEY}"),
        ])
        .unwrap(),
        prices: Prices::parse(&["mistral-embed=0.1", "pricey=100000"]).unwrap(),
        budget: budget.parse().unwrap(),
        provider: provider.clone(),
        store: UsageStore::in_memory().unwrap(),
        clock: Arc::new(move || *clock_now.lock().unwrap()),
    });
    Harness {
        gateway,
        provider,
        now,
    }
}

impl Harness {
    async fn post(&self, key: Option<&str>, body: Value) -> (StatusCode, Option<String>, Value) {
        let mut request =
            Request::post("/v1/embeddings").header(header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = llm_gateway::api::router(self.gateway.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, retry_after, serde_json::from_slice(&bytes).unwrap())
    }

    fn calls(&self) -> usize {
        self.provider.calls.load(Ordering::SeqCst)
    }

    fn set_now(&self, when: &str) {
        *self.now.lock().unwrap() = at(when);
    }
}

fn at(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn request(sphere: &str, model: &str, inputs: &[&str]) -> Value {
    json!({ "sphere": sphere, "model": model, "inputs": inputs })
}

#[tokio::test]
async fn answers_and_records_usage_per_caller_sphere_provider_and_model() {
    let h = harness("10");

    let (status, _, body) = h
        .post(
            Some(MNEMOGRAPH_KEY),
            request("work", "mistral-embed", &["a", "bcd"]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"], "mistral-embed");
    assert_eq!(body["embeddings"], json!([[1.0], [3.0]]));
    assert_eq!(body["usage"]["input_tokens"], 20);
    // 20 tokens at 0.1 EUR per million.
    assert_eq!(body["usage"]["cost_eur"], 0.000002);

    let (status, _, _) = h
        .post(
            Some(DOCUMENTS_KEY),
            request("private", "mistral-embed", &["x"]),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        h.gateway.store.usage().unwrap(),
        vec![
            Usage {
                at: at("2026-10-31T12:00:00Z"),
                caller: "mnemograph".into(),
                sphere: "work".into(),
                provider: "fake".into(),
                model: "mistral-embed".into(),
                inputs: 2,
                input_tokens: 20,
                cost: Eur::from_nano(2_000),
            },
            Usage {
                at: at("2026-10-31T12:00:00Z"),
                caller: "documents".into(),
                sphere: "private".into(),
                provider: "fake".into(),
                model: "mistral-embed".into(),
                inputs: 1,
                input_tokens: 10,
                cost: Eur::from_nano(1_000),
            },
        ]
    );
}

#[tokio::test]
async fn refuses_unauthenticated_callers() {
    let h = harness("10");
    for key in [None, Some("wrong-key-0123456789"), Some("")] {
        let (status, _, body) = h.post(key, request("work", "mistral-embed", &["a"])).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthenticated");
    }
    assert_eq!(h.calls(), 0);
    assert!(h.gateway.store.usage().unwrap().is_empty());
}

#[tokio::test]
async fn refuses_invalid_requests_without_calling_the_provider() {
    let h = harness("10");
    let cases = [
        (request("work", "gpt-5", &["a"]), "unknown_model"),
        (request("work", "mistral-embed", &[]), "invalid_request"),
        (request("Work!", "mistral-embed", &["a"]), "invalid_request"),
        (
            json!({ "sphere": "work", "inputs": ["a"] }),
            "invalid_request",
        ),
        (
            json!({ "sphere": "work", "model": "mistral-embed", "inputs": ["a"], "extra": 1 }),
            "invalid_request",
        ),
    ];
    for (body, code) in cases {
        let (status, _, answer) = h.post(Some(MNEMOGRAPH_KEY), body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(answer["error"]["code"], code, "{body}");
    }
    assert_eq!(h.calls(), 0);
}

#[tokio::test]
async fn reports_provider_failures_and_records_no_usage() {
    let h = harness("10");
    *h.provider.fail.lock().unwrap() = true;
    let (status, _, body) = h
        .post(
            Some(MNEMOGRAPH_KEY),
            request("work", "mistral-embed", &["a"]),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "provider_failure");
    assert_eq!(h.calls(), 1);
    assert!(h.gateway.store.usage().unwrap().is_empty());
}

#[tokio::test]
async fn refuses_when_the_budget_is_used_up_until_the_next_month() {
    // 10 tokens of `pricey` cost 1 EUR; the budget allows two such calls.
    let h = harness("2");
    let pricey = || request("work", "pricey", &["a"]);

    for _ in 0..2 {
        let (status, _, body) = h.post(Some(MNEMOGRAPH_KEY), pricey()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["usage"]["cost_eur"], 1.0);
    }

    // Exhausted for every caller, sphere and model, without contacting the provider.
    for (key, sphere, model) in [
        (MNEMOGRAPH_KEY, "work", "pricey"),
        (DOCUMENTS_KEY, "private", "mistral-embed"),
    ] {
        let (status, retry_after, body) = h.post(Some(key), request(sphere, model, &["a"])).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "budget_exhausted");
        assert_eq!(body["error"]["retry_at"], "2026-11-01T00:00:00Z");
        // From 12:00 on 31 October to midnight UTC.
        assert_eq!(retry_after.as_deref(), Some("43200"));
    }
    assert_eq!(h.calls(), 2);
    assert_eq!(
        h.gateway.store.refusals().unwrap(),
        vec![
            Refusal {
                at: at("2026-10-31T12:00:00Z"),
                caller: "mnemograph".into(),
                sphere: "work".into(),
                model: "pricey".into(),
                reason: "budget_exhausted".into(),
            },
            Refusal {
                at: at("2026-10-31T12:00:00Z"),
                caller: "documents".into(),
                sphere: "private".into(),
                model: "mistral-embed".into(),
                reason: "budget_exhausted".into(),
            },
        ]
    );

    // Still refused in the last moment of the month.
    h.set_now("2026-10-31T23:59:59.999Z");
    let (status, _, _) = h.post(Some(MNEMOGRAPH_KEY), pricey()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // A new month brings a new budget.
    h.set_now("2026-11-01T00:00:00Z");
    for _ in 0..2 {
        let (status, _, body) = h.post(Some(MNEMOGRAPH_KEY), pricey()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _, body) = h.post(Some(MNEMOGRAPH_KEY), pricey()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["retry_at"], "2026-12-01T00:00:00Z");
    assert_eq!(h.calls(), 4);
}

#[tokio::test]
async fn a_call_may_overrun_the_budget_once() {
    // The cost is known only after the call, so the call that crosses the budget is answered.
    let h = harness("0.5");
    let (status, _, _) = h
        .post(Some(MNEMOGRAPH_KEY), request("work", "pricey", &["a"]))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.gateway
            .store
            .spent_since(at("2026-10-01T00:00:00Z"))
            .unwrap(),
        Eur::from_nano(1_000_000_000)
    );
    let (status, _, _) = h
        .post(
            Some(MNEMOGRAPH_KEY),
            request("work", "mistral-embed", &["a"]),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}
