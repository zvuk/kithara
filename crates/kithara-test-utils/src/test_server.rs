//! Entry point for a spec-driven test server.
//!
//! Route families:
//! - `GET /assets/{path...}` — static test assets
//! - `GET /signal/{form}/{spec_with_ext}` — procedural signal generation
//! - `GET /stream/{hls_spec}` — HLS stream generation

use std::env;

use axum::Router;
use tower_http::cors::CorsLayer;
use url::Url;

use crate::http_server::TestHttpServer;
use crate::routes::{assets, signal, stream};

/// In-process unified test server with RAII shutdown.
pub struct TestServerHelper {
    server: TestHttpServer,
}

impl TestServerHelper {
    /// Spawn the unified server on a random localhost port.
    pub async fn new() -> Self {
        let server = TestHttpServer::new(router()).await;
        Self { server }
    }

    /// Build a URL for a static test asset.
    ///
    /// ```ignore
    /// let url = helper.asset("hls/master.m3u8");
    /// // → http://127.0.0.1:{port}/assets/hls/master.m3u8
    /// ```
    #[must_use]
    pub fn asset(&self, name: &str) -> Url {
        let trimmed = name.trim_start_matches('/');
        self.server.url(&format!("/assets/{trimmed}"))
    }

    /// Build an arbitrary URL on this server.
    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        self.server.url(path)
    }

    /// Base URL of this server.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        self.server.base_url()
    }
}

/// Start the server as a standalone process (used by the `test_server` binary).
pub async fn run_test_server() {
    let port: u16 = env::var("TEST_SERVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3444);

    let mut server = TestHttpServer::bind(&format!("0.0.0.0:{port}"), router()).await;
    println!("test server listening on {}", server.base_url());
    server.completion().await;
}

fn router() -> Router {
    Router::new()
        .merge(assets::router())
        .merge(signal::router())
        .merge(stream::router())
        .layer(CorsLayer::permissive())
}
