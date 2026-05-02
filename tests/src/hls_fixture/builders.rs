//! Reusable HLS stream builders to reduce test setup boilerplate.
//!
//! Most HLS tests follow the same pattern:
//! 1. Create `TestServer`
//! 2. Build `HlsConfig` with `StoreOptions` + `CancellationToken` + `AbrMode`
//! 3. Create `Stream<Hls>`
//!
//! This module provides [`HlsStreamBuilder`] to condense those steps.

use std::path::Path;

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    stream::Stream,
};
use kithara_test_utils::hls_fixture::TestServer;
use tokio_util::sync::CancellationToken;

/// Builder for creating `Stream<Hls>` in integration tests.
///
/// Defaults to `Manual(0)` ABR and `/master.m3u8` master playlist.
///
/// # Examples
///
/// ```rust,ignore
/// let server = TestServer::new().await;
/// let mut stream = HlsStreamBuilder::new()
///     .build(&server, temp_dir.path(), cancel_token)
///     .await;
/// ```
pub struct HlsStreamBuilder {
    master_path: &'static str,
    initial_abr_mode: AbrMode,
    store_subdir: Option<&'static str>,
    max_assets: Option<usize>,
    max_bytes: Option<u64>,
}

impl HlsStreamBuilder {
    pub fn new() -> Self {
        Self {
            master_path: "/master.m3u8",
            initial_abr_mode: AbrMode::Manual(0),
            store_subdir: None,
            max_assets: None,
            max_bytes: None,
        }
    }

    /// Set the ABR variant index (default: `Manual(0)`).
    pub fn variant(mut self, variant: usize) -> Self {
        self.initial_abr_mode = AbrMode::Manual(variant);
        self
    }

    /// Override the initial ABR mode entirely.
    pub fn abr_mode(mut self, mode: AbrMode) -> Self {
        self.initial_abr_mode = mode;
        self
    }

    /// Use master playlist with init segments (`/master-init.m3u8`).
    pub fn with_init(mut self) -> Self {
        self.master_path = "/master-init.m3u8";
        self
    }

    /// Use encrypted master playlist (`/master-encrypted.m3u8`).
    pub fn with_encrypted(mut self) -> Self {
        self.master_path = "/master-encrypted.m3u8";
        self
    }

    /// Use a subdirectory under `temp_dir` for the asset store.
    pub fn store_subdir(mut self, subdir: &'static str) -> Self {
        self.store_subdir = Some(subdir);
        self
    }

    /// Set the maximum number of cached assets in the store.
    pub fn max_assets(mut self, max_assets: usize) -> Self {
        self.max_assets = Some(max_assets);
        self
    }

    /// Set the maximum total bytes for cached assets in the store.
    pub fn max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }

    /// Build the `Stream<Hls>` from the configured options.
    pub async fn build(
        self,
        server: &TestServer,
        temp_path: &Path,
        cancel_token: CancellationToken,
    ) -> Stream<Hls> {
        let url = server.url(self.master_path);

        let store_path = match self.store_subdir {
            Some(sub) => temp_path.join(sub),
            None => temp_path.to_path_buf(),
        };

        let mut store_opts = StoreOptions::new(&store_path);
        if let Some(max) = self.max_assets {
            store_opts = store_opts.with_max_assets(max);
        }
        if let Some(max) = self.max_bytes {
            store_opts = store_opts.with_max_bytes(max);
        }

        let config = HlsConfig::new(url)
            .with_store(store_opts)
            .with_cancel(cancel_token)
            .with_initial_abr_mode(self.initial_abr_mode);

        Stream::<Hls>::new(config)
            .await
            .expect("HLS stream creation")
    }
}

impl Default for HlsStreamBuilder {
    fn default() -> Self {
        Self::new()
    }
}
