//! Entry point for spec-driven test server.
//!
//! Route families:
//! - `GET /assets/{path...}` — static test assets
//! - `GET /signal/sawtooth/{spec}.{ext}` — procedural signal generation
//! - `GET /stream/{hls_spec}.m3u8` — synthetic HLS master playlist
//! - `GET /stream/{hls_spec}/v{v}.m3u8` — media playlist
//! - `GET /stream/{hls_spec}/init/v{v}.mp4` — init segment
//! - `GET /stream/{hls_spec}/seg/v{v}_{s}.m4s` — media segment

use crate::routes::{assets, signal, stream};
use axum::Router;
use std::env;
use tower_http::cors::CorsLayer;

pub async fn run_test_server() {
    use tokio::net::TcpListener;

    let port: u16 = env::var("TEST_SERVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3444);

    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await.expect("bind listener");
    println!("test server listening on {addr}");

    axum::serve(listener, router()).await.expect("run server");
}

fn router() -> Router {
    Router::new()
        .merge(assets::router())
        .merge(signal::router())
        .merge(stream::router())
        .layer(CorsLayer::permissive())
}
