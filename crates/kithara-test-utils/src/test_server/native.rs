use std::{env, sync::Arc};

use axum::{Router, routing::get};
use tower_http::cors::CorsLayer;
use url::Url;

use crate::{
    hls_url::HlsSpec,
    http_server::TestHttpServer,
    routes::{assets, signal, stream},
    signal_spec::{SignalKind as InternalSignalKind, parse_signal_request},
    signal_url::{SignalKind, SignalSpec, signal_path},
    test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder},
    test_server_state::TestServerState,
};

/// In-process unified test server with RAII shutdown.
pub struct TestServerHelper {
    state: Arc<TestServerState>,
    server: TestHttpServer,
}

impl TestServerHelper {
    /// Spawn the unified server on a random localhost port.
    pub async fn new() -> Self {
        let state = TestServerState::new();
        let server = TestHttpServer::new(router(Arc::clone(&state))).await;
        Self { state, server }
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

    /// Build a URL for a static test asset.
    #[must_use]
    pub fn asset(&self, name: &str) -> Url {
        let trimmed = name.trim_start_matches('/');
        self.server.url(&format!("/assets/{trimmed}"))
    }

    /// Build a URL for `/signal/sawtooth/...`.
    #[must_use]
    pub async fn sawtooth(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::Sawtooth, spec)
    }

    /// Build a URL for `/signal/sawtooth-desc/...`.
    #[must_use]
    pub async fn sawtooth_descending(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::SawtoothDescending, spec)
    }

    /// Build a URL for `/signal/sine/...`.
    #[must_use]
    pub async fn sine(&self, spec: &SignalSpec, freq_hz: f64) -> Url {
        self.signal_url(SignalKind::Sine { freq_hz }, spec)
    }

    /// Build a URL for `/signal/silence/...`.
    #[must_use]
    pub async fn silence(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::Silence, spec)
    }

    /// Register an HLS fixture from a builder, storing media blobs in the server.
    ///
    /// # Errors
    ///
    /// Returns [`CreateHlsError`] if inserting the resolved spec fails.
    pub async fn create_hls(
        &self,
        builder: HlsFixtureBuilder,
    ) -> Result<CreatedHls, CreateHlsError> {
        let spec =
            builder.into_spec_with_blob_registrar(|bytes| self.state.register_hls_blob(bytes));
        self.create_hls_from_spec(spec).await
    }

    pub(crate) async fn create_hls_from_spec(
        &self,
        spec: HlsSpec,
    ) -> Result<CreatedHls, CreateHlsError> {
        let token = self.state.insert_hls_spec(spec)?;
        Ok(CreatedHls::new(self.base_url().clone(), token))
    }

    fn signal_url(&self, kind: SignalKind, spec: &SignalSpec) -> Url {
        let path = signal_path(kind, spec);
        let prefix = format!("/signal/{}/", kind.path_segment());
        let spec_with_ext = path
            .strip_prefix(&prefix)
            .expect("signal path must match kind prefix");
        let internal_kind =
            InternalSignalKind::try_from(kind.path_segment()).expect("valid signal route");
        let request =
            parse_signal_request(internal_kind, spec_with_ext).expect("valid signal spec");
        let token = self.state.insert_signal(request);
        self.url(&format!(
            "/signal/{}/{}.{}",
            kind.path_segment(),
            token,
            spec.format.path_ext()
        ))
    }
}

async fn health() -> &'static str {
    "ok"
}

/// Start the server as a standalone process (used by the `test_server` binary).
pub async fn run_test_server() {
    let port: u16 = env::var("TEST_SERVER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3444);
    let state = TestServerState::new();
    let mut server = TestHttpServer::bind(&format!("127.0.0.1:{port}"), router(state)).await;
    println!("test server listening on {}", server.base_url());
    server.completion().await;
}

fn router(state: Arc<TestServerState>) -> Router {
    Router::<Arc<TestServerState>>::new()
        .route("/health", get(health))
        .merge(assets::router())
        .merge(signal::router())
        .merge(stream::router())
        .merge(crate::routes::token::router())
        .layer(CorsLayer::permissive())
        .with_state(state)
}
