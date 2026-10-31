//! LLM providers behind the gateway, and Mistral as the first of them.

use std::future::Future;
use std::pin::Pin;

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
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

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
        let client = genai::Client::new().map_err(|err| ProviderError(err.to_string()))?;
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
                .map_err(|err| ProviderError(err.to_string()))?;

            let mut embeddings = response.embeddings;
            if embeddings.len() != inputs.len() {
                return Err(ProviderError(format!(
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
                .ok_or_else(|| ProviderError("Mistral reported no token usage".into()))?;
            Ok(Embeddings {
                vectors: embeddings.into_iter().map(|e| e.vector).collect(),
                input_tokens: u64::try_from(input_tokens).unwrap_or(0),
            })
        })
    }
}
