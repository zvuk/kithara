use url::Url;

use crate::{
    hls_url::HlsSpec,
    server_url::join_server_url,
    signal_url::{SignalKind, SignalSpec, signal_path},
    test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder, post_token},
    token_store::{TokenRequest, TokenRoute},
};

/// Client-side handle for the externally managed unified test server.
pub struct TestServerHelper {
    base_url: Url,
}

impl TestServerHelper {
    /// Connect to the external unified server used by WASM tests.
    pub async fn new() -> Self {
        Self {
            base_url: external_test_server_url(),
        }
    }

    /// Build an arbitrary URL on this server.
    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        join_server_url(&self.base_url, path)
    }

    /// Base URL of this server.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Build a URL for a static test asset.
    #[must_use]
    pub fn asset(&self, name: &str) -> Url {
        let trimmed = name.trim_start_matches('/');
        self.url(&format!("/assets/{trimmed}"))
    }

    /// Build a URL for `/signal/sawtooth/...`.
    #[must_use]
    pub async fn sawtooth(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::Sawtooth, spec).await
    }

    /// Build a URL for `/signal/sawtooth-desc/...`.
    #[must_use]
    pub async fn sawtooth_descending(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::SawtoothDescending, spec).await
    }

    /// Build a URL for `/signal/sine/...`.
    #[must_use]
    pub async fn sine(&self, spec: &SignalSpec, freq_hz: f64) -> Url {
        self.signal_url(SignalKind::Sine { freq_hz }, spec).await
    }

    /// Build a URL for `/signal/sweep/...`.
    #[must_use]
    pub async fn sweep(
        &self,
        spec: &SignalSpec,
        start_hz: f64,
        end_hz: f64,
        mode: crate::signal_url::SweepMode,
    ) -> Url {
        self.signal_url(
            SignalKind::Sweep {
                start_hz,
                end_hz,
                mode,
            },
            spec,
        )
        .await
    }

    /// Build a URL for `/signal/silence/...`.
    #[must_use]
    pub async fn silence(&self, spec: &SignalSpec) -> Url {
        self.signal_url(SignalKind::Silence, spec).await
    }

    pub async fn create_hls(
        &self,
        builder: HlsFixtureBuilder,
    ) -> Result<CreatedHls, CreateHlsError> {
        let spec = builder.into_inline_spec();
        self.create_hls_from_spec(spec).await
    }

    pub(crate) async fn create_hls_from_spec(
        &self,
        spec: HlsSpec,
    ) -> Result<CreatedHls, CreateHlsError> {
        let request = TokenRequest {
            route: TokenRoute::Hls,
            signal_kind: None,
            signal_spec_with_ext: None,
            hls_spec: Some(spec),
        };
        let token = post_token(&self.base_url, &request).await?;
        Ok(CreatedHls::new(self.base_url.clone(), token))
    }

    async fn signal_url(&self, kind: SignalKind, spec: &SignalSpec) -> Url {
        let path = signal_path(kind, spec);
        let prefix = format!("/signal/{}/", kind.path_segment());
        let spec_with_ext = path
            .strip_prefix(&prefix)
            .expect("signal path must match kind prefix");
        let request = TokenRequest {
            route: TokenRoute::Signal,
            signal_kind: Some(kind.path_segment().to_string()),
            signal_spec_with_ext: Some(spec_with_ext.to_string()),
            hls_spec: None,
        };
        let token = post_token(&self.base_url, &request)
            .await
            .expect("signal token registration must succeed");
        self.url(&format!(
            "/signal/{}/{}.{}",
            kind.path_segment(),
            token,
            spec.format.path_ext()
        ))
    }
}

fn external_test_server_url() -> Url {
    let base = option_env!("TEST_SERVER_URL").unwrap_or("http://127.0.0.1:3444");
    Url::parse(base).expect("valid TEST_SERVER_URL")
}
