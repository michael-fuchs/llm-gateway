//! The HTTP API.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{EmbedError, EmbedRequest, Gateway};

/// The largest request body accepted.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub fn router(gateway: Arc<Gateway>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/embeddings", post(embeddings))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(gateway)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingsRequest {
    sphere: String,
    model: String,
    inputs: Vec<String>,
}

#[derive(Serialize)]
struct EmbeddingsResponse {
    model: String,
    embeddings: Vec<Vec<f32>>,
    usage: UsageResponse,
}

#[derive(Serialize)]
struct UsageResponse {
    input_tokens: u64,
    cost_eur: f64,
}

async fn embeddings(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
    body: Result<Json<EmbeddingsRequest>, JsonRejection>,
) -> Result<Json<EmbeddingsResponse>, ApiError> {
    let caller = bearer(&headers)
        .and_then(|key| gateway.callers.authenticate(key))
        .ok_or(ApiError::Unauthenticated)?
        .to_string();
    let Json(body) = body.map_err(|rejection| ApiError::InvalidRequest(rejection.body_text()))?;

    let model = body.model.clone();
    let embedded = gateway
        .embed(EmbedRequest {
            caller,
            sphere: body.sphere,
            model: body.model,
            inputs: body.inputs,
        })
        .await?;
    Ok(Json(EmbeddingsResponse {
        model,
        embeddings: embedded.embeddings.vectors,
        usage: UsageResponse {
            input_tokens: embedded.embeddings.input_tokens,
            cost_eur: embedded.cost.as_f64(),
        },
    }))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, key) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(key.trim())
}

/// An error answer: a status, and a JSON body `{"error": {"code", "message", ...}}` whose
/// `code` callers act on.
#[derive(Debug)]
enum ApiError {
    Unauthenticated,
    InvalidRequest(String),
    Embed(EmbedError),
}

impl From<EmbedError> for ApiError {
    fn from(err: EmbedError) -> Self {
        ApiError::Embed(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = match &self {
            ApiError::Unauthenticated => "a valid caller key is required as a bearer token".into(),
            ApiError::InvalidRequest(message) => message.clone(),
            ApiError::Embed(err) => err.to_string(),
        };
        let mut headers = HeaderMap::new();
        let mut error = json!({ "message": message });
        let (status, code) = match &self {
            ApiError::Unauthenticated => {
                headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                (StatusCode::UNAUTHORIZED, "unauthenticated")
            }
            ApiError::InvalidRequest(_) | ApiError::Embed(EmbedError::InvalidRequest(_)) => {
                (StatusCode::BAD_REQUEST, "invalid_request")
            }
            ApiError::Embed(EmbedError::UnknownModel(_)) => {
                (StatusCode::BAD_REQUEST, "unknown_model")
            }
            ApiError::Embed(EmbedError::UnknownSphere(_)) => {
                (StatusCode::BAD_REQUEST, "unknown_sphere")
            }
            ApiError::Embed(EmbedError::SphereRule { .. }) => {
                (StatusCode::FORBIDDEN, "sphere_rule")
            }
            ApiError::Embed(EmbedError::RateLimited { retry_after, .. }) => {
                // Whole seconds, rounded up, so a caller that waits this long gets through.
                let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
                headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
                error["retry_after_seconds"] = json!(seconds);
                (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
            }
            ApiError::Embed(EmbedError::BudgetExhausted {
                until, retry_after, ..
            }) => {
                headers.insert(header::RETRY_AFTER, HeaderValue::from(*retry_after));
                error["retry_at"] = json!(until.to_string());
                error["retry_after_seconds"] = json!(retry_after);
                (StatusCode::TOO_MANY_REQUESTS, "budget_exhausted")
            }
            ApiError::Embed(EmbedError::Provider(err)) => {
                tracing::warn!(error = %err, "provider call failed");
                (StatusCode::BAD_GATEWAY, "provider_failure")
            }
            ApiError::Embed(EmbedError::Store(err)) => {
                tracing::error!(error = %err, "usage database failed");
                error["message"] = json!("internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        error["code"] = json!(code);
        (status, headers, Json(json!({ "error": error }))).into_response()
    }
}
