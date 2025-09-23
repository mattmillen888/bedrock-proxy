use axum::{routing::post, Router};
use std::{net::SocketAddr, sync::Arc};
use tracing::info;

mod handlers;
mod signing;
mod state;
mod transform;

use handlers::{invoke_handler, invoke_stream_handler};
use state::AppState;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let state = AppState::from_env();

    let app = Router::new()
        .route("/invoke", post(invoke_handler))
        .route("/invoke_stream", post(invoke_stream_handler))
        .with_state(Arc::new(state));

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    info!("🚀 Bedrock proxy running at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
