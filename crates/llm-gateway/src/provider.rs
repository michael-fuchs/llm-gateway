//! LLM providers behind the gateway, and Mistral as the first of them.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use genai::adapter::AdapterKind;
use genai::resolver::{AuthData, Endpoint};
use genai::{ModelIden, ServiceTarget};

/// Embeddings for a batch of texts, in the order of the texts.
#[derive(Debug, Clone, PartialEq)]
pub struct Embeddings {
    pub vectors: Vec<Vec<f32>>,
    /// Input tokens the provider counted, which is what it charges for.
    pub input_tokens: u64,
}

/// A provider call that failed: the provider refused or did not answer properly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProviderError {
    pub message: String,
    /// Whether the same call may succeed later (rate limited, overloaded, no answer).
    pub transient: bool,
    /// How long the provider asked to wait before trying again.
    pub retry_after: Option<Duration>,
}

impl ProviderError {
    /// A failure that may go away: worth trying again.
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
            retry_after: None,
        }
    }

    /// A failure that repeats on every try, such as a rejected key or request.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
            retry_after: None,
        }
    }

    pub fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }
}

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Embeddings, ProviderError>> + Send + 'a>>;

/// An LLM provider the gateway sends calls to.
pub trait Provider: Send + Sync {
    /// The provider's name in usage records, e.g. `mistral`.
    fn name(&self) -> &str;

    /// Embeds `inputs` with `model`.
    fn embed<'a>(&'a self, model: &'a str, inputs: &'a [String]) -> ProviderFuture<'a>;
}

/// Mistral's API, called through the `genai` crate.
pub struct Mistral {
    client: genai::Client,
    endpoint: Endpoint,
    api_key: String,
}

impl Mistral {
    pub const DEFAULT_URL: &str = "https://api.mistral.ai/v1/";

    /// A client for Mistral's API at `url` (its base URL, ending in `/v1/`).
    pub fn new(api_key: String, url: &str) -> Result<Self, ProviderError> {
        let url = if url.ends_with('/') {
            url.to_string()
        } else {
            format!("{url}/")
        };
        let client =
            genai::Client::new().map_err(|err| ProviderError::permanent(err.to_string()))?;
        Ok(Self {
            client,
            endpoint: Endpoint::from_owned(url),
            api_key,
        })
    }
}

impl Provider for Mistral {
    fn name(&self) -> &str {
        "mistral"
    }

    fn embed<'a>(&'a self, model: &'a str, inputs: &'a [String]) -> ProviderFuture<'a> {
        Box::pin(async move {
            // An explicit target bypasses genai's guessing of provider, endpoint and key.
            let target = ServiceTarget {
                endpoint: self.endpoint.clone(),
                auth: AuthData::from_single(self.api_key.clone()),
                model: ModelIden::new(AdapterKind::Mistral, model),
            };
            let response = self
                .client
                .embed_batch(target, inputs.to_vec(), None)
                .await
                .map_err(classify)?;

            let mut embeddings = response.embeddings;
            if embeddings.len() != inputs.len() {
                return Err(ProviderError::permanent(format!(
                    "Mistral returned {} embeddings for {} inputs",
                    embeddings.len(),
                    inputs.len()
                )));
            }
            embeddings.sort_by_key(|e| e.index);
            let input_tokens = response
                .usage
                .prompt_tokens
                .or(response.usage.total_tokens)
                .ok_or_else(|| ProviderError::permanent("Mistral reported no token usage"))?;
            Ok(Embeddings {
                vectors: embeddings.into_iter().map(|e| e.vector).collect(),
                input_tokens: u64::try_from(input_tokens).unwrap_or(0),
            })
        })
    }
}

/// Sorts a genai error into transient and permanent failures.
///
/// Rate limits (429), timeouts (408) and server errors (5xx) are transient, as are failures to
/// reach the provider at all. Every other status (a rejected key or request) and answers that
/// cannot be read are permanent: trying again would fail the same way.
fn classify(err: genai::Error) -> ProviderError {
    let message = err.to_string();
    if let Some(status) = err.status() {
        let code = status.as_u16();
        return if code == 429 || code == 408 || status.is_server_error() {
            ProviderError::transient(message).with_retry_after(err.headers().and_then(retry_after))
        } else {
            ProviderError::permanent(message)
        };
    }
    match err {
        genai::Error::WebModelCall {
            webc_error: genai::webc::Error::Reqwest(_),
            ..
        } => ProviderError::transient(message),
        _ => ProviderError::permanent(message),
    }
}

/// The wait a provider asks for in `retry-after-ms` or `retry-after` (in seconds).
fn retry_after(headers: &axum::http::HeaderMap) -> Option<Duration> {
    let number = |name: &str| -> Option<f64> {
        let value: f64 = headers.get(name)?.to_str().ok()?.trim().parse().ok()?;
        (value.is_finite() && value >= 0.0).then_some(value)
    };
    number("retry-after-ms")
        .map(|ms| Duration::from_secs_f64(ms / 1000.0))
        .or_else(|| number("retry-after").map(Duration::from_secs_f64))
}
