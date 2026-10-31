//! The identity provider for the owner's browser login: OpenID Connect discovery, the
//! provider's signing keys, and verification of ID tokens.
//!
//! Adapted from mnemograph's `auth` module (only the browser-login half; the gateway has no
//! agent tokens). Both move into the assistant's shared crates later.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

/// Unknown key IDs trigger a JWKS refresh, but not more often than this.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(60);
const CLOCK_LEEWAY_SECS: u64 = 30;
/// The token type of OAuth access tokens in JWT form (RFC 9068), which are not ID tokens.
const ACCESS_TOKEN_TYPE: &str = "at+jwt";

/// The provider endpoints the browser login uses.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub authorization: String,
    pub token: String,
}

/// What the gateway reads from an ID token after verifying it.
#[derive(Debug, Clone, Deserialize)]
pub struct IdTokenClaims {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    authorization_endpoint: String,
    token_endpoint: String,
}

/// The identity provider, found by discovery.
pub struct Provider {
    issuer: String,
    endpoints: Endpoints,
    jwks_url: String,
    http: reqwest::Client,
    keys: RwLock<KeyCache>,
}

#[derive(Default)]
struct KeyCache {
    keys: HashMap<String, (DecodingKey, Algorithm)>,
    fetched_at: Option<Instant>,
}

impl Provider {
    /// Looks up the issuer's endpoints via OpenID Connect discovery and loads its keys.
    pub async fn discover(issuer: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let discovery: Discovery = http
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("fetching {url}"))?
            .json()
            .await
            .with_context(|| format!("reading {url}"))?;
        // Providers differ on the trailing slash (Rauthy's issuer has one); tokens carry
        // exactly the provider's own form.
        anyhow::ensure!(
            discovery.issuer.trim_end_matches('/') == issuer.trim_end_matches('/'),
            "the provider at {url} calls itself {:?}, not {issuer:?}",
            discovery.issuer
        );
        let provider = Self {
            issuer: discovery.issuer,
            endpoints: Endpoints {
                authorization: discovery.authorization_endpoint,
                token: discovery.token_endpoint,
            },
            jwks_url: discovery.jwks_uri,
            http,
            keys: RwLock::default(),
        };
        provider.refresh_keys(true).await?;
        Ok(provider)
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Verifies an ID token: signature, issuer, audience (the web client's ID) and expiry.
    /// The caller checks the nonce.
    pub async fn verify_id_token(
        &self,
        token: &str,
        client_id: &str,
    ) -> anyhow::Result<IdTokenClaims> {
        let header = jsonwebtoken::decode_header(token).context("invalid ID token")?;
        anyhow::ensure!(
            !header
                .typ
                .as_deref()
                .is_some_and(|typ| typ.eq_ignore_ascii_case(ACCESS_TOKEN_TYPE)),
            "an access token is not an ID token"
        );
        let kid = header.kid.context("the ID token names no signing key")?;
        let (key, algorithm) = self
            .key(&kid)
            .await
            .context("the ID token's signing key is unknown")?;
        anyhow::ensure!(
            header.alg == algorithm,
            "the ID token's algorithm does not match its signing key"
        );
        let mut validation = Validation::new(algorithm);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[client_id]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = CLOCK_LEEWAY_SECS;
        Ok(
            jsonwebtoken::decode::<IdTokenClaims>(token, &key, &validation)
                .context("invalid ID token")?
                .claims,
        )
    }

    async fn key(&self, kid: &str) -> Option<(DecodingKey, Algorithm)> {
        if let Some(key) = self.keys.read().await.keys.get(kid) {
            return Some(key.clone());
        }
        // The provider may have rotated its keys.
        if let Err(err) = self.refresh_keys(false).await {
            tracing::warn!(%err, "refreshing the JWKS failed");
        }
        self.keys.read().await.keys.get(kid).cloned()
    }

    async fn refresh_keys(&self, force: bool) -> anyhow::Result<()> {
        let mut cache = self.keys.write().await;
        if !force
            && cache
                .fetched_at
                .is_some_and(|at| at.elapsed() < JWKS_MIN_REFRESH)
        {
            return Ok(());
        }
        cache.fetched_at = Some(Instant::now());
        let jwks: JwkSet = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("fetching {}", self.jwks_url))?
            .json()
            .await
            .with_context(|| format!("reading {}", self.jwks_url))?;
        cache.keys = jwks
            .keys
            .iter()
            .filter_map(|jwk| {
                let kid = jwk.common.key_id.clone()?;
                let algorithm = jwk.common.key_algorithm?.to_string().parse().ok()?;
                let key = DecodingKey::from_jwk(jwk).ok()?;
                Some((kid, (key, algorithm)))
            })
            .collect();
        tracing::info!(keys = cache.keys.len(), "loaded signing keys");
        Ok(())
    }
}
