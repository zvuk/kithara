//! Test asset store helpers
//!
//! Provides `TestAssets` and helper functions for creating test assets.

use std::sync::Arc;

use kithara::assets::{AssetStore, AssetStoreBuilder, EvictConfig, ProcessChunkFn};
use kithara::drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara::net::{HttpClient, NetOptions};
use rstest::fixture;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Wrapper for test assets with temp directory lifetime management
pub(crate) struct TestAssets {
    assets: AssetStore<DecryptContext>,
    _temp_dir: Arc<TempDir>,
}

impl TestAssets {
    pub(crate) fn assets(&self) -> &AssetStore<DecryptContext> {
        &self.assets
    }
}

/// Create test assets with default "test-hls" root
pub(crate) fn create_test_assets() -> TestAssets {
    create_test_assets_with_root("test-hls")
}

/// Create test assets with custom asset root
pub(crate) fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    let temp_dir = TempDir::new().unwrap();
    let temp_dir = Arc::new(temp_dir);

    let drm_fn: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
            aes128_cbc_process_chunk(input, output, ctx, is_last)
        });

    let assets = AssetStoreBuilder::new()
        .process_fn(drm_fn)
        .root_dir(temp_dir.path().to_path_buf())
        .asset_root(Some(asset_root))
        .evict_config(EvictConfig::default())
        .cancel(CancellationToken::new())
        .build_disk();

    TestAssets {
        assets,
        _temp_dir: temp_dir,
    }
}

/// Create test HTTP client with default options
pub(crate) fn create_test_net() -> HttpClient {
    let net_opts = NetOptions::default();
    HttpClient::new(net_opts)
}

/// Fixture: test assets
#[fixture]
pub(crate) fn assets_fixture() -> TestAssets {
    create_test_assets()
}

/// Fixture: test HTTP client
#[fixture]
pub(crate) fn net_fixture() -> HttpClient {
    create_test_net()
}

/// Fixture: both assets and network client
#[fixture]
pub(crate) fn abr_cache_and_net() -> (TestAssets, HttpClient) {
    (create_test_assets(), create_test_net())
}
