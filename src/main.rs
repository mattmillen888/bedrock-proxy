use axum::{
    routing::{any, get, post},
    Router,
};
use std::{net::SocketAddr, sync::Arc};

mod handlers;
mod signing;
mod state;
mod transform;

use handlers::{
    catch_all_handler, invoke_handler, invoke_stream_handler, models_handler,
    openai_chat_completions_handler,
};
use state::AppState;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let state = AppState::from_env();

    let app = Router::new()
        // Legacy endpoints (for backward compatibility)
        .route("/invoke", post(invoke_handler))
        .route("/invoke_stream", post(invoke_stream_handler))
        // OpenAI-compatible endpoints
        .route(
            "/v1/chat/completions",
            post(openai_chat_completions_handler),
        )
        .route("/v1/models", get(models_handler))
        .fallback(any(catch_all_handler))
        .with_state(Arc::new(state));

    let addr = SocketAddr::from(([127, 0, 0, 1], 9678));
    println!("🚀 Bedrock proxy running at http://{}", addr);
    tracing::info!("🔧 Server starting with debug logging enabled");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
