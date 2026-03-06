//! Asset server helper for integration tests.
//!
//! On native targets this spawns a local HTTP server serving repository assets.
//! On wasm32 it points at the fixture server started by the wasm test runner.

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use axum::Router;
#[cfg(not(target_arch = "wasm32"))]
use tower_http::{cors::CorsLayer, services::ServeDir};
use url::Url;

#[cfg(not(target_arch = "wasm32"))]
use crate::http_server::TestHttpServer;

/// Lightweight asset server handle shared by native and wasm tests.
pub struct AssetServer {
    base_url: Url,
    #[cfg(not(target_arch = "wasm32"))]
    _server: TestHttpServer,
}

impl AssetServer {
    /// Join a path to the asset server base URL.
    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        self.base_url
            .join(path)
            .expect("join asset server URL path")
    }

    /// Base URL of this asset server.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }
}

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
#[cfg(not(target_arch = "wasm32"))]
pub async fn serve_assets() -> AssetServer {
    let assets = assets_dir();
    let router = Router::new()
        .nest_service("/hls", ServeDir::new(assets.join("hls")))
        .nest_service("/drm", ServeDir::new(assets.join("drm")))
        .nest_service("/track.mp3", ServeDir::new(assets.join("track.mp3")))
        .layer(CorsLayer::permissive());
    let server = TestHttpServer::new(router).await;
    let base_url = server.base_url().clone();

    AssetServer {
        base_url,
        _server: server,
    }
}

/// Resolve the repository-root `assets/` directory.
///
/// `CARGO_MANIFEST_DIR` points to `crates/kithara-test-utils/`,
/// so we go two levels up to reach the repo root.
#[cfg(not(target_arch = "wasm32"))]
fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("parent of kithara-test-utils")
        .parent()
        .expect("repo root")
        .join("assets")
}

/// Use the fixture server started by the wasm test runner.
#[cfg(target_arch = "wasm32")]
pub async fn serve_assets() -> AssetServer {
    let base_url = crate::fixture_client::fixture_server_url()
        .parse()
        .expect("parse fixture server URL");

    AssetServer { base_url }
}
