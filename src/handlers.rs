use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use base64::Engine;
use futures_util::{Stream, StreamExt};
use http::Request;
use serde_json::{json, Value};
use std::{pin::Pin, sync::Arc};
use tracing::{debug, error, info};

use crate::{
    signing::sign_request,
    state::AppState,
    transform::{
        bedrock_chunk_to_openai, bedrock_to_openai, openai_to_bedrock, transform_payload,
        OpenAIRequest,
    },
};

// Helper function to extract JSON from AWS event stream chunks
fn extract_json_from_bedrock_chunk(chunk_text: &str) -> Option<Value> {
    // AWS sends binary event stream format. We need to find JSON within the binary data
    // Look for JSON objects that contain the "bytes" field

    // Try to find JSON patterns in the chunk
    let mut start_idx = 0;
    while let Some(json_start) = chunk_text[start_idx..].find('{') {
        let actual_start = start_idx + json_start;

        // Try to find the end of this JSON object by counting braces
        let mut brace_count = 0;
        let mut in_string = false;
        let mut escaped = false;
        let mut json_end = None;

        for (i, c) in chunk_text[actual_start..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }

            match c {
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                '{' if !in_string => brace_count += 1,
                '}' if !in_string => {
                    brace_count -= 1;
                    if brace_count == 0 {
                        json_end = Some(actual_start + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }

        if let Some(end) = json_end {
            let json_str = &chunk_text[actual_start..end];
            debug!("📋 Found JSON in chunk: {}", json_str);

            if let Ok(chunk_data) = serde_json::from_str::<Value>(json_str) {
                if let Some(bytes_b64) = chunk_data.get("bytes").and_then(|b| b.as_str()) {
                    // Decode base64 and parse JSON
                    if let Ok(decoded_bytes) = base64::prelude::BASE64_STANDARD.decode(bytes_b64) {
                        if let Ok(decoded_str) = String::from_utf8(decoded_bytes) {
                            debug!("🔓 Decoded chunk: {}", decoded_str);
                            if let Ok(bedrock_json) = serde_json::from_str(&decoded_str) {
                                return Some(bedrock_json);
                            }
                        }
                    }
                }
            }
            start_idx = end;
        } else {
            break;
        }
    }

    None
}

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

    // Sign the request
    let reqwest_req = match sign_request(http_req, &state, true) {
        Ok(r) => r,
        Err(e) => {
            // Convert error to string to ensure it's Send
            let error_msg = e.to_string();
            let err_stream: Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> =
                Box::pin(futures_util::stream::once(async move {
                    Ok(Event::default().data(format!("Signing error: {}", error_msg)))
                }));
            return Sse::new(err_stream);
        }
    };

    let raw_stream: Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> = Box::pin(
        async_stream::stream! {
            match state.client.execute(reqwest_req).await {
                Ok(resp) => {
                    let status = resp.status();
                    info!("🌊 Stream response status: {}", status);

                    if !status.is_success() {
                        if let Ok(text) = resp.text().await {
                            error!("❌ Stream error: {}", text);
                            yield Ok(Event::default().data(format!("Error {}: {}", status, text)));
                        }
                        return;
                    }

                    let mut stream = resp.bytes_stream();

                    while let Some(chunk_result) = stream.next().await {
                        match chunk_result {
                            Ok(bytes) => {
                                let text = String::from_utf8_lossy(&bytes);
                                debug!("📦 Raw chunk: {:?}", text);

                                // Extract JSON from the event stream format
                                if let Some(json_chunk) = extract_json_from_bedrock_chunk(&text) {
                                    if let Some(openai_chunk) = bedrock_chunk_to_openai(&json_chunk) {
                                        yield Ok(Event::default().data(
                                            serde_json::to_string(&openai_chunk).unwrap()
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                yield Ok(Event::default().data(format!("Stream error: {}", e)));
                                break;
                            }
                        }
                    }

                    // Final [DONE] event for SSE clients
                    yield Ok(Event::default().data("[DONE]"));
                }
                Err(e) => {
                    error!("❌ Request error: {}", e);
                    yield Ok(Event::default().data(format!("Request error: {}", e)));
                }
            }
        },
    );

    Sse::new(raw_stream).keep_alive(KeepAlive::default())
}

pub async fn models_handler(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
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

pub async fn openai_chat_completions_handler(
    State(state): State<Arc<AppState>>,
    Json(openai_req): Json<OpenAIRequest>,
) -> impl IntoResponse {
    info!(
        "🤖 OpenAI chat completions request (stream={}, messages={})",
        openai_req.stream.unwrap_or(false),
        openai_req.messages.len()
    );
    debug!("📝 Request payload: {}", serde_json::to_string_pretty(&openai_req).unwrap_or_else(|_| "Failed to serialize".to_string()));

    if openai_req.stream == Some(true) {
        let stream_response =
            openai_chat_completions_stream_handler(State(state), Json(openai_req)).await;
        return stream_response.into_response();
    }

    let model = openai_req.model.as_deref().unwrap_or("claude-sonnet-4");
    let bedrock_payload = openai_to_bedrock(&openai_req);
    debug!("🔄 Transformed to Bedrock payload: {}", serde_json::to_string_pretty(&bedrock_payload).unwrap_or_else(|_| "Failed to serialize".to_string()));

    let endpoint = format!(
        "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
        state.region, state.inference_profile
    );

    let body = serde_json::to_vec(&bedrock_payload).unwrap();
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

    debug!("🌐 Making request to Bedrock endpoint: {}", endpoint);
    match state.client.execute(reqwest_req).await {
        Ok(resp) => {
            let status = resp.status();
            debug!("📡 Bedrock response status: {}", status);
            match resp.text().await {
                Ok(text) => {
                    debug!("📨 Bedrock response body: {}", text);
                    if status.is_success() {
                        if let Ok(bedrock_response) = serde_json::from_str::<Value>(&text) {
                            debug!("✅ Successfully parsed Bedrock response");
                            let openai_response = bedrock_to_openai(&bedrock_response, model);
                            debug!("🔄 Converted to OpenAI format: {}", serde_json::to_string_pretty(&openai_response).unwrap_or_else(|_| "Failed to serialize".to_string()));
                            Json(openai_response).into_response()
                        } else {
                            error!("❌ Failed to parse Bedrock response as JSON: {}", text);
                            (status, text).into_response()
                        }
                    } else {
                        error!("❌ Bedrock API error {}: {}", status, text);
                        (status, text).into_response()
                    }
                }
                Err(e) => {
                    error!("❌ Failed to read response body: {}", e);
                    (
                        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to read response: {}", e),
                    )
                        .into_response()
                }
            }
        }
        Err(e) => {
            error!("❌ HTTP request failed: {}", e);
            (
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Request error: {}", e),
            )
                .into_response()
        }
    }
}

pub async fn openai_chat_completions_stream_handler(
    State(state): State<Arc<AppState>>,
    Json(openai_req): Json<OpenAIRequest>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>>> {
    let model = openai_req
        .model
        .as_deref()
        .unwrap_or("claude-sonnet-4")
        .to_string();
    let bedrock_payload = openai_to_bedrock(&openai_req);

    let endpoint = format!(
        "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke-with-response-stream",
        state.region, state.inference_profile
    );

    let body = serde_json::to_vec(&bedrock_payload).unwrap();
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
            let error_msg = e.to_string();
            let err_stream: Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> =
                Box::pin(futures_util::stream::once(async move {
                    Ok(Event::default().data(format!("Signing error: {}", error_msg)))
                }));
            return Sse::new(err_stream);
        }
    };

    let raw_stream: Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>> = Box::pin(
        async_stream::stream! {
            match state.client.execute(reqwest_req).await {
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        yield Ok(Event::default().data(format!("Error {}: {}", status, text)));
                        yield Ok(Event::default().data("[DONE]"));
                        return;
                    }

                    let mut stream = resp.bytes_stream();

                    let mut sent_first = false;

                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(bytes) => {
                                let text = String::from_utf8_lossy(&bytes);
                                debug!("📦 Raw chunk: {:?}", text);

                                if let Some(json_chunk) = extract_json_from_bedrock_chunk(&text) {
                                    if let Some(openai_chunk) = bedrock_chunk_to_openai(&json_chunk) {
                                        yield Ok(Event::default().data(serde_json::to_string(&openai_chunk).unwrap()));
                                        sent_first = true;
                                    }
                                }
                            }
                            Err(e) => {
                                yield Ok(Event::default().data(format!("Stream error: {}", e)));
                                break;
                            }
                        }
                    }

                    // Ensure at least one chunk
                    if !sent_first {
                        let dummy = json!({
                            "id": format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                            "object": "chat.completion.chunk",
                            "created": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {"content": ""},
                                "finish_reason": null
                            }]
                        });
                        yield Ok(Event::default().data(dummy.to_string()));
                    }

                    yield Ok(Event::default().data("[DONE]"));
                }
                Err(e) => {
                    yield Ok(Event::default().data(format!("Request error: {}", e)));
                    yield Ok(Event::default().data("[DONE]"));
                }
            }
        },
    );

    Sse::new(raw_stream).keep_alive(KeepAlive::default())
}

pub async fn catch_all_handler(
    uri: axum::http::Uri,
    method: axum::http::Method,
) -> impl IntoResponse {
    info!("🔍 Unhandled request: {} {}", method, uri);
    axum::http::StatusCode::NOT_FOUND
}
