//! Test asset store helpers
//!
//! Provides `TestAssets` and helper functions for creating test assets.

use std::sync::Arc;

use kithara_assets::{AssetStore, AssetStoreBuilder, EvictConfig};
use kithara_net::{HttpClient, NetOptions};
use rstest::fixture;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Wrapper for test assets with temp directory lifetime management
pub struct TestAssets {
    assets: AssetStore,
    _temp_dir: Arc<TempDir>,
}

impl TestAssets {
    #[allow(dead_code)]
    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }
}

/// Create test assets with default "test-hls" root
pub fn create_test_assets() -> TestAssets {
    create_test_assets_with_root("test-hls")
}

/// Create test assets with custom asset root
pub fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    let temp_dir = TempDir::new().unwrap();
    let temp_dir = Arc::new(temp_dir);

    let assets = AssetStoreBuilder::new()
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
pub fn create_test_net() -> HttpClient {
    let net_opts = NetOptions::default();
    HttpClient::new(net_opts)
}

/// Fixture: test assets
#[fixture]
pub fn assets_fixture() -> TestAssets {
    create_test_assets()
}

/// Fixture: test HTTP client
#[fixture]
pub fn net_fixture() -> HttpClient {
    create_test_net()
}

/// Fixture: both assets and network client
#[fixture]
pub fn abr_cache_and_net() -> (TestAssets, HttpClient) {
    (create_test_assets(), create_test_net())
}
