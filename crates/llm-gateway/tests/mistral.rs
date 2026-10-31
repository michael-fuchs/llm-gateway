//! The Mistral provider through `genai`: against a fake Mistral API, and (ignored) against
//! the real one.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use llm_gateway::{Mistral, Provider};
use serde_json::{Value, json};

const KEY: &str = "fake-mistral-key";

/// A request as the fake received it: its Authorization header and its body.
type Received = (Option<String>, Value);

#[derive(Clone, Default)]
struct FakeMistral {
    requests: Arc<Mutex<Vec<Received>>>,
    fail: bool,
}

/// Answers like Mistral's `POST /v1/embeddings`, with the embeddings deliberately out of order.
async fn embeddings(
    State(fake): State<FakeMistral>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let auth = headers
        .get(header::AUTHORIZATION)
        .map(|v| v.to_str().unwrap().to_string());
    fake.requests.lock().unwrap().push((auth, body.clone()));
    if fake.fail {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "object": "error", "message": "overloaded" })),
        );
    }
    let inputs = body["input"].as_array().unwrap();
    let mut data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(index, text)| {
            json!({
                "object": "embedding",
                "embedding": [index as f32, text.as_str().unwrap().len() as f32],
                "index": index,
            })
        })
        .collect();
    data.reverse();
    (
        StatusCode::OK,
        Json(json!({
            "id": "embd-1",
            "object": "list",
            "data": data,
            "model": "mistral-embed",
            "usage": { "prompt_tokens": 7, "total_tokens": 7, "completion_tokens": 0 },
        })),
    )
}

async fn start(fake: FakeMistral) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embeddings))
        .with_state(fake);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn sends_mistral_requests_and_reads_its_answers() {
    let fake = FakeMistral::default();
    let url = start(fake.clone()).await;
    let mistral = Mistral::new(KEY.into(), &url).unwrap();

    let inputs = vec!["a".to_string(), "bcd".to_string()];
    let embeddings = mistral.embed("mistral-embed", &inputs).await.unwrap();
    assert_eq!(embeddings.vectors, vec![vec![0.0, 1.0], vec![1.0, 3.0]]);
    assert_eq!(embeddings.input_tokens, 7);

    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0.as_deref(), Some("Bearer fake-mistral-key"));
    // Mistral rejects fields it does not know, so only these may be sent.
    assert_eq!(
        requests[0].1,
        json!({ "model": "mistral-embed", "input": ["a", "bcd"] })
    );
}

#[tokio::test]
async fn reports_mistral_errors() {
    let fake = FakeMistral {
        fail: true,
        ..Default::default()
    };
    let url = start(fake).await;
    let mistral = Mistral::new(KEY.into(), url.trim_end_matches('/')).unwrap();
    let err = mistral
        .embed("mistral-embed", &["a".to_string()])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("503"), "{err}");
}

/// Calls the real Mistral API: `LLM_GATEWAY_MISTRAL_API_KEY=... cargo test -- --ignored`.
#[tokio::test]
#[ignore = "calls the real Mistral API and needs LLM_GATEWAY_MISTRAL_API_KEY"]
async fn embeds_with_the_real_mistral_api() {
    let Ok(key) = std::env::var("LLM_GATEWAY_MISTRAL_API_KEY") else {
        eprintln!("LLM_GATEWAY_MISTRAL_API_KEY is not set; skipping");
        return;
    };
    let mistral = Mistral::new(key, Mistral::DEFAULT_URL).unwrap();
    let inputs = vec![
        "The gateway holds the provider keys.".to_string(),
        "Das Gateway hält die Schlüssel der Anbieter.".to_string(),
    ];
    let embeddings = mistral.embed("mistral-embed", &inputs).await.unwrap();
    assert_eq!(embeddings.vectors.len(), 2);
    assert_eq!(embeddings.vectors[0].len(), 1024);
    assert!(embeddings.input_tokens > 0);
}
