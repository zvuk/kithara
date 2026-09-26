use std::path::Path;

use kithara::{
    abr::{AbrMode, VariantIndex},
    assets::{AssetStore, StorageBackend},
    hls::{Hls, HlsConfig},
    platform::CancelToken,
    stream::Stream,
};
use url::Url;

use crate::bufpool_ext::{TestPools, pools};

/// Builder for creating `Stream<Hls>` in integration tests.
///
/// Defaults to `Manual(0)` ABR.
///
/// # Examples
///
/// ```rust,ignore
/// let mut stream = HlsStreamBuilder::new()
///     .build(hls.master_url(), temp_dir.path(), cancel_token)
///     .await;
/// ```
pub struct HlsStreamBuilder {
    initial_abr_mode: AbrMode,
    store_subdir: Option<&'static str>,
    max_assets: Option<usize>,
    max_bytes: Option<u64>,
}

impl HlsStreamBuilder {
    pub const fn new() -> Self {
        Self {
            initial_abr_mode: AbrMode::Manual(VariantIndex::new(0)),
            store_subdir: None,
            max_assets: None,
            max_bytes: None,
        }
    }

    /// Set the ABR variant index (default: `Manual(0)`).
    pub const fn variant(mut self, variant: usize) -> Self {
        self.initial_abr_mode = AbrMode::Manual(VariantIndex::new(variant));
        self
    }

    /// Override the initial ABR mode entirely.
    pub const fn abr_mode(mut self, mode: AbrMode) -> Self {
        self.initial_abr_mode = mode;
        self
    }

    /// Use a subdirectory under `temp_dir` for the asset store.
    pub const fn store_subdir(mut self, subdir: &'static str) -> Self {
        self.store_subdir = Some(subdir);
        self
    }

    /// Set the maximum number of cached assets in the store.
    pub const fn max_assets(mut self, max_assets: usize) -> Self {
        self.max_assets = Some(max_assets);
        self
    }

    /// Set the maximum total bytes for cached assets in the store.
    pub const fn max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }

    /// Build the `Stream<Hls>` from the configured options.
    pub async fn build(
        self,
        master_url: Url,
        temp_path: &Path,
        cancel: CancelToken,
    ) -> Stream<Hls<TestPools>> {
        let pools = pools();

        let store_path = match self.store_subdir {
            Some(sub) => temp_path.join(sub),
            None => temp_path.to_path_buf(),
        };

        let store_opts = AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk { root: store_path })
            .maybe_max_assets(self.max_assets)
            .maybe_max_bytes(self.max_bytes)
            .build();

        let config = HlsConfig::for_url(master_url)
            .store(store_opts)
            .pools(pools)
            .cancel(cancel)
            .initial_abr_mode(self.initial_abr_mode)
            .build();

        Stream::<Hls<TestPools>>::new(config)
            .await
            .expect("HLS stream creation")
    }
}

impl Default for HlsStreamBuilder {
    fn default() -> Self {
        Self::new()
    }
}
