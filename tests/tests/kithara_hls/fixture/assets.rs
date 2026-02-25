//! Test asset store helpers
//!
//! Provides `TestAssets` and helper functions for creating test assets.
//! On native: disk-backed with temp directory. On WASM: ephemeral (in-memory).

use std::sync::Arc;

use kithara::{
    assets::{AssetStore, AssetStoreBuilder, ProcessChunkFn},
    drm::{DecryptContext, aes128_cbc_process_chunk},
    internal::FetchManager,
    net::{HttpClient, NetOptions},
};
use tokio_util::sync::CancellationToken;

/// Wrapper for test assets with temp directory lifetime management
pub(crate) struct TestAssets {
    assets: AssetStore<DecryptContext>,
    #[cfg(not(target_arch = "wasm32"))]
    _temp_dir: Arc<kithara_test_utils::TestTempDir>,
}

impl TestAssets {
    pub(crate) fn assets(&self) -> &AssetStore<DecryptContext> {
        &self.assets
    }
}

fn drm_process_fn() -> ProcessChunkFn<DecryptContext> {
    Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
        aes128_cbc_process_chunk(input, output, ctx, is_last)
    })
}

/// Create test assets with default "test-hls" root
pub(crate) fn create_test_assets() -> TestAssets {
    create_test_assets_with_root("test-hls")
}

/// Create test assets with custom asset root
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    use kithara::assets::EvictConfig;

    let temp_dir = kithara_test_utils::TestTempDir::new();
    let temp_dir = Arc::new(temp_dir);

    let assets = AssetStoreBuilder::new()
        .process_fn(drm_process_fn())
        .root_dir(temp_dir.path().to_path_buf())
        .asset_root(Some(asset_root))
        .evict_config(EvictConfig::default())
        .cancel(CancellationToken::new())
        .build();

    TestAssets {
        assets,
        _temp_dir: temp_dir,
    }
}

/// Create test assets with custom asset root (WASM: ephemeral in-memory store)
#[cfg(target_arch = "wasm32")]
pub(crate) fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    let assets = AssetStoreBuilder::new()
        .process_fn(drm_process_fn())
        .asset_root(Some(asset_root))
        .cancel(CancellationToken::new())
        .build();

    TestAssets { assets }
}

/// Create test HTTP client with default options
pub(crate) fn create_test_net() -> HttpClient {
    let net_opts = NetOptions::default();
    HttpClient::new(net_opts)
}

pub(crate) fn test_fetch_manager(assets: &TestAssets, net: HttpClient) -> FetchManager<HttpClient> {
    FetchManager::new(assets.assets().clone(), net, CancellationToken::new())
}

pub(crate) fn test_fetch_manager_shared(
    assets: &TestAssets,
    net: HttpClient,
) -> Arc<FetchManager<HttpClient>> {
    Arc::new(test_fetch_manager(assets, net))
}

/// Fixture: test assets
#[kithara::fixture]
pub(crate) fn assets_fixture() -> TestAssets {
    create_test_assets()
}

/// Fixture: test HTTP client
#[kithara::fixture]
pub(crate) fn net_fixture() -> HttpClient {
    create_test_net()
}
