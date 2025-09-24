use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use futures_util::Stream;
use http::Request;
use serde_json::Value;
use std::{pin::Pin, sync::Arc};
use tracing::{debug, error, info};

use crate::{
    signing::sign_request,
    state::AppState,
    transform::transform_payload,
};

pub async fn invoke_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let endpoint = format!(
        "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
        state.region, state.inference_profile
    );

    let transformed_payload = transform_payload(payload);

    debug!(
        "📊 Sending payload: {}",
        serde_json::to_string_pretty(&transformed_payload).unwrap()
    );

    let body = serde_json::to_vec(&transformed_payload).unwrap();

    let http_req = Request::builder()
        .method("POST")
        .uri(&endpoint)
        .header("Content-Type", "application/json")
        .body(body)
        .unwrap();

    let reqwest_req = match sign_request(http_req, &state, false) {
        Ok(r) => r,
        Err(e) => {
            return (
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Signing error: {}", e),
            )
                .into_response()
        }
    };

    match state.client.execute(reqwest_req).await {
        Ok(resp) => {
            let status = resp.status();
            match resp.text().await {
                Ok(text) => {
                    info!("📨 Response status: {}", status);
                    debug!("📨 Response body: {}", text);

                    if status.is_success() {
                        if let Ok(json) = serde_json::from_str::<Value>(&text) {
                            Json(json).into_response()
                        } else {
                            (status, text).into_response()
                        }
                    } else {
                        (status, text).into_response()
                    }
                }
                Err(e) => (
                    reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to read response: {}", e),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Request error: {}", e),
        )
            .into_response(),
    }
}

pub async fn invoke_stream_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>>> {
    let endpoint = format!(
        "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke-with-response-stream",
        state.region, state.inference_profile
    );

    let transformed_payload = transform_payload(payload);

    debug!(
        "🌊 Streaming payload: {}",
        serde_json::to_string_pretty(&transformed_payload).unwrap()
    );

    let body = serde_json::to_vec(&transformed_payload).unwrap();

    let http_req = Request::builder()
        .method("POST")
        .uri(&endpoint)
        .header("Content-Type", "application/json")
        .header("X-Amzn-Bedrock-Accept", "application/json")
        .body(body)
        .unwrap();

    let reqwest_req = match sign_request(http_req, &state, true) {
        Ok(r) => r,
        Err(e) => {
            let error_msg = format!("Signing error: {}", e);
            let err_stream: Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> =
                Box::pin(futures_util::stream::once(async move {
                    Ok(Event::default().data(error_msg))
                }));
            return Sse::new(err_stream);
        }
    };

    let raw_stream: Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> =
        Box::pin(async_stream::stream! {
            match state.client.execute(reqwest_req).await {
                Ok(resp) => {
                    let status = resp.status();
                    info!("🌊 Stream response status: {}", status);

                    if !status.is_success() {
                        if let Ok(text) = resp.text().await {
                            error!("❌ Stream error: {}", text);
                            yield Ok(Event::default().data(format!("Error {}: {}", status, text)));
                        }
                    } else {
                        let mut stream = resp.bytes_stream();
                        use futures_util::StreamExt;

                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(bytes) => {
                                    let text = String::from_utf8_lossy(&bytes);
                                    debug!("📦 Raw chunk: {:?}", text);
                                    yield Ok(Event::default().data(text.to_string()));
                                }
                                Err(e) => {
                                    yield Ok(Event::default().data(format!("Stream error: {}", e)));
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("❌ Request error: {}", e);
                    yield Ok(Event::default().data(format!("Request error: {}", e)));
                }
            }
        });

    Sse::new(raw_stream).keep_alive(KeepAlive::default())
}

pub async fn models_handler(
    State(_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    info!("📋 Models endpoint called");

    let models = serde_json::json!({
        "object": "list",
        "data": [
            {
                "id": "claude-sonnet-4",
                "object": "model",
                "created": 1677610602,
                "owned_by": "anthropic",
                "permission": [],
                "root": "claude-sonnet-4",
                "parent": null
            }
        ]
    });

    Json(models)
}

pub async fn catch_all_handler(uri: axum::http::Uri, method: axum::http::Method) -> impl IntoResponse {
    info!("🔍 Unhandled request: {} {}", method, uri);
    axum::http::StatusCode::NOT_FOUND
}