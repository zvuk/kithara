//! Local asset server for native integration tests.
//!
//! Serves files from the repository `assets/` directory over HTTP,
//! removing the need for an external streaming server in tests.

use std::path::PathBuf;

use axum::Router;
use tower_http::{cors::CorsLayer, services::ServeDir};

use crate::http_server::TestHttpServer;

/// Spawn a local HTTP server that serves the repository `assets/` directory.
///
/// Routes:
/// - `/hls/...`     — HLS playlists and segments
/// - `/drm/...`     — DRM-encrypted HLS content
/// - `/track.mp3`   — progressive MP3 file
///
/// # Panics
///
/// Panics if the `assets/` directory cannot be located.
pub async fn serve_assets() -> TestHttpServer {
    let assets = assets_dir();
    let router = Router::new()
        .nest_service("/hls", ServeDir::new(assets.join("hls")))
        .nest_service("/drm", ServeDir::new(assets.join("drm")))
        .nest_service("/track.mp3", ServeDir::new(assets.join("track.mp3")))
        .layer(CorsLayer::permissive());
    TestHttpServer::new(router).await
}

/// Resolve the repository-root `assets/` directory.
///
/// `CARGO_MANIFEST_DIR` points to `crates/kithara-test-utils/`,
/// so we go two levels up to reach the repo root.
fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("parent of kithara-test-utils")
        .parent()
        .expect("repo root")
        .join("assets")
}
