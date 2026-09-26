use kithara::platform::sync::Arc;
use kithara_test_fixtures::SignalAsset;
use url::Url;

use crate::{
    server_url::join_server_url,
    test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder, post_token},
    token_store::TokenRequest,
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

    /// Build a URL for a static test asset.
    #[must_use]
    pub fn asset(&self, name: &str) -> Url {
        let trimmed = name.trim_start_matches('/');
        self.url(&format!("/assets/{trimmed}"))
    }

    /// Base URL of this server.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub async fn create_hls(
        &self,
        builder: HlsFixtureBuilder,
    ) -> Result<CreatedHls, CreateHlsError> {
        let fixture = Arc::new(builder.clone());
        let request = TokenRequest {
            hls_spec: builder.into_inline_spec(),
        };
        let token = post_token(&self.base_url, &request).await?;
        Ok(CreatedHls::new(self.base_url.clone(), token, fixture))
    }

    /// URL of one build-time generated signal body.
    #[must_use]
    pub fn signal(&self, asset: SignalAsset) -> Url {
        self.url(&asset.path())
    }

    /// Build an arbitrary URL on this server.
    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        join_server_url(&self.base_url, path)
    }
}

fn external_test_server_url() -> Url {
    let base = option_env!("TEST_SERVER_URL").unwrap_or("http://127.0.0.1:3444");
    Url::parse(base).expect("valid TEST_SERVER_URL")
}
