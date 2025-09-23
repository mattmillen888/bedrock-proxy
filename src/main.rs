use aws_sigv4::http_request::{
    sign, SignableBody, SignableRequest, SigningParams as HttpSigningParams, SigningSettings,
};
use aws_sigv4::sign::v4::SigningParams;
use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::post,
    Json, Router,
};
use futures_util::Stream;
use http::Request;
use reqwest::Client;
use serde_json::{json, Value};
use std::{net::SocketAddr, pin::Pin, sync::Arc, time::SystemTime};

#[derive(Clone)]
struct AppState {
    client: Client,
    region: String,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    inference_profile: String,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let state = AppState {
        client: Client::new(),
        region: std::env::var("AWS_REGION").expect("AWS_REGION must be set"),
        access_key: std::env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID must be set"),
        secret_key: std::env::var("AWS_SECRET_ACCESS_KEY")
            .expect("AWS_SECRET_ACCESS_KEY must be set"),
        session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
        inference_profile: std::env::var("INFERENCE_PROFILE")
            .unwrap_or_else(|_| "apac.anthropic.claude-sonnet-4-20250514-v1:0".to_string()),
    };

    let app = Router::new()
        .route("/invoke", post(invoke_handler))
        .route("/invoke_stream", post(invoke_stream_handler))
        .with_state(Arc::new(state));

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("🚀 Bedrock proxy running at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// 🔧 Sign request and convert to reqwest::Request
fn sign_request(
    req: Request<Vec<u8>>,
    state: &AppState,
    is_streaming: bool,
) -> Result<reqwest::Request, Box<dyn std::error::Error>> {
    let identity = aws_credential_types::Credentials::new(
        &state.access_key,
        &state.secret_key,
        state.session_token.clone(),
        None,
        "hardcoded-credentials",
    )
    .into();

    let signing_settings = SigningSettings::default();

    let signing_params: HttpSigningParams = SigningParams::builder()
        .identity(&identity)
        .region(&state.region)
        .name("bedrock") // ✅ Service name for signing
        .time(SystemTime::now())
        .settings(signing_settings)
        .build()
        .unwrap()
        .into();

    let host_header = format!("bedrock-runtime.{}.amazonaws.com", state.region);

    // Build headers list for signing
    let mut headers = vec![
        ("content-type", "application/json"),
        ("host", host_header.as_str()),
    ];

    // Add streaming headers if needed
    let accept_header = if is_streaming {
        "application/vnd.amazon.eventstream"
    } else {
        "application/json"
    };
    headers.push(("accept", accept_header));

    // Create the signable request
    let signable_req = SignableRequest::new(
        req.method().as_str(),
        req.uri().to_string(),
        headers.clone().into_iter(),
        SignableBody::Bytes(req.body()),
    )?;

    let (signing_instructions, _signature) = sign(signable_req, &signing_params)?.into_parts();

    // Build http::Request first to apply signing instructions
    let mut signed_http = http::Request::builder()
        .method(req.method().clone())
        .uri(req.uri().clone())
        .header("content-type", "application/json")
        .header("accept", accept_header)
        .body(req.body().clone())?;

    signing_instructions.apply_to_request_http1x(&mut signed_http);

    // Convert to reqwest::Request
    let mut builder = state.client.request(
        reqwest::Method::from_bytes(req.method().as_str().as_bytes())?,
        req.uri().to_string(),
    );

    // Copy all headers from the signed request
    for (k, v) in signed_http.headers().iter() {
        builder = builder.header(k, v);
    }

    builder = builder.body(signed_http.body().clone());

    Ok(builder.build()?)
}

/// 📝 Transform request payload to correct format for Claude models
fn transform_payload(mut payload: Value) -> Value {
    // If it has 'prompt' field, convert to messages format
    if let Some(prompt) = payload.get("prompt").and_then(|p| p.as_str()) {
        payload = json!({
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": payload.get("max_tokens_to_sample").unwrap_or(&json!(200))
        });
    }

    // Ensure max_tokens exists (rename from max_tokens_to_sample if present)
    if payload.get("max_tokens_to_sample").is_some() && payload.get("max_tokens").is_none() {
        if let Some(max_tokens) = payload.get("max_tokens_to_sample") {
            payload["max_tokens"] = max_tokens.clone();
            payload
                .as_object_mut()
                .unwrap()
                .remove("max_tokens_to_sample");
        }
    }

    // Add the correct anthropic_version for Claude 4
    payload["anthropic_version"] = json!("bedrock-2023-05-31");

    payload
}

/// 📤 Standard invoke (JSON response)
async fn invoke_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    // Use configurable inference profile for Claude 4 (required for on-demand throughput)
    let endpoint = format!(
        "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
        state.region, state.inference_profile
    );

    let transformed_payload = transform_payload(payload);

    println!(
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
                    println!("📨 Response status: {}", status);
                    println!("📨 Response body: {}", text);

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

/// 🌊 Streaming invoke (SSE response)
async fn invoke_stream_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, axum::Error>> + Send>>> {
    // Use configurable inference profile for Claude 4 (required for on-demand throughput)
    let endpoint = format!(
        "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke-with-response-stream",
        state.region, state.inference_profile
    );

    let transformed_payload = transform_payload(payload);

    println!(
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
                    println!("🌊 Stream response status: {}", status);

                    if !status.is_success() {
                        // Handle error response
                        if let Ok(text) = resp.text().await {
                            println!("❌ Stream error: {}", text);
                            yield Ok(Event::default().data(format!("Error {}: {}", status, text)));
                        }
                    } else {
                        // Stream the response chunks
                        let mut stream = resp.bytes_stream();
                        use futures_util::StreamExt;

                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(bytes) => {
                                    let text = String::from_utf8_lossy(&bytes);
                                    println!("📦 Raw chunk: {:?}", text);
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
                    println!("❌ Request error: {}", e);
                    yield Ok(Event::default().data(format!("Request error: {}", e)));
                }
            }
        });

    Sse::new(raw_stream).keep_alive(KeepAlive::default())
}
