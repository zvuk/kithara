//! Test asset store helpers
//!
//! Provides `TestAssets` and helper functions for creating test assets.
//! On native: disk-backed with temp directory. On WASM: ephemeral (in-memory).

use std::sync::Arc;

use kithara::{
    assets::{AssetStore, AssetStoreBuilder, ProcessChunkFn},
    drm::{DecryptContext, aes128_cbc_process_chunk},
    internal::{KeyManager, PlaylistCache},
    net::{HttpClient, NetOptions},
};
use kithara_stream::dl::{Downloader, DownloaderConfig, Peer, PeerHandle};
use kithara_test_utils::TestTempDir;
use tokio_util::sync::CancellationToken;

/// Wrapper for test assets with temp directory lifetime management
pub struct TestAssets {
    assets: AssetStore<DecryptContext>,
    #[cfg(not(target_arch = "wasm32"))]
    _temp_dir: Arc<TestTempDir>,
}

impl TestAssets {
    pub fn assets(&self) -> &AssetStore<DecryptContext> {
        &self.assets
    }
}

fn drm_process_fn() -> ProcessChunkFn<DecryptContext> {
    Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
        aes128_cbc_process_chunk(input, output, ctx, is_last)
    })
}

/// Create test assets with default "test-hls" root
pub fn create_test_assets() -> TestAssets {
    create_test_assets_with_root("test-hls")
}

/// Create test assets with custom asset root
#[cfg(not(target_arch = "wasm32"))]
pub fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    use kithara::assets::EvictConfig;

    let temp_dir = TestTempDir::new();
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
pub fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    let assets = AssetStoreBuilder::new()
        .process_fn(drm_process_fn())
        .asset_root(Some(asset_root))
        .cancel(CancellationToken::new())
        .build();

    TestAssets { assets }
}

/// Create test HTTP client with default options
pub fn create_test_net() -> HttpClient {
    let net_opts = NetOptions::default();
    HttpClient::new(net_opts)
}

/// Create a private test [`Downloader`] with a fresh cancel token.
pub fn create_test_downloader() -> Downloader {
    Downloader::new(DownloaderConfig::default())
}

/// Create a private test [`PeerHandle`] via `Downloader::register`.
fn create_test_peer_handle() -> PeerHandle {
    struct TestPeer;
    impl Peer for TestPeer {}
    let cancel = CancellationToken::new();
    let dl = Downloader::new(DownloaderConfig::default().with_cancel(cancel.child_token()));
    dl.register(Arc::new(TestPeer))
}

/// Build a test [`PlaylistCache`] backed by the supplied
/// [`TestAssets`] + a fresh private [`PeerHandle`].
pub fn test_playlist_cache(assets: &TestAssets, _net: HttpClient) -> PlaylistCache {
    PlaylistCache::new(assets.assets().clone(), create_test_peer_handle())
}

/// Build a test [`KeyManager`] backed by a fresh [`PeerHandle`] and
/// the supplied [`TestAssets`]. Mirrors the production constructor in
/// `Hls::create` so integration tests exercise the same wiring.
pub fn test_key_manager(
    assets: &TestAssets,
    key_registry: Option<kithara_drm::KeyProcessorRegistry>,
) -> KeyManager {
    KeyManager::new(
        create_test_peer_handle(),
        assets.assets().clone(),
        None,
        key_registry,
    )
}

/// Fixture: test assets
#[kithara::fixture]
pub fn assets_fixture() -> TestAssets {
    create_test_assets()
}

/// Fixture: test HTTP client
#[kithara::fixture]
pub fn net_fixture() -> HttpClient {
    create_test_net()
}
