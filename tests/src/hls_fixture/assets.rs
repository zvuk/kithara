#[cfg(not(target_arch = "wasm32"))]
use kithara::assets::StorageBackend;
use kithara::{
    assets::{AssetScope, AssetStore, AssetStoreBuilder},
    hls::{KeyStore, PlaylistCache},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc},
    stream::dl::{Downloader, DownloaderConfig, Peer, PeerHandle},
};

use crate::TestTempDir;

/// Wrapper for test assets with temp directory lifetime management
pub struct TestAssets {
    assets: AssetStore,
    asset_root: Arc<str>,
    #[cfg(not(target_arch = "wasm32"))]
    _temp_dir: Arc<TestTempDir>,
}

impl TestAssets {
    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }

    /// Scope bound to this fixture's `asset_root`, mirroring how
    /// `Hls::create` scopes its per-stream store.
    pub fn scope(&self) -> AssetScope {
        self.assets.scope(Arc::clone(&self.asset_root))
    }
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

    let assets = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .evict_config(EvictConfig::default())
        .cancel(CancelToken::never())
        .build();

    TestAssets {
        assets,
        asset_root: Arc::from(asset_root),
        _temp_dir: temp_dir,
    }
}

/// Create test assets with custom asset root (WASM: ephemeral in-memory store)
#[cfg(target_arch = "wasm32")]
pub fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    let assets = AssetStoreBuilder::default()
        .cancel(CancelToken::never())
        .build();

    TestAssets {
        assets,
        asset_root: Arc::from(asset_root),
    }
}

/// Create test HTTP client with default options
pub fn create_test_net() -> HttpClient {
    HttpClient::new(NetOptions::default(), CancelToken::never())
}

/// Create a private test [`Downloader`] with a fresh cancel token.
pub fn create_test_downloader() -> Downloader {
    Downloader::new(DownloaderConfig::for_client(create_test_net()).build())
}

/// Create a private test [`PeerHandle`] via `Downloader::register`.
fn create_test_peer_handle() -> PeerHandle {
    struct TestPeer;
    impl kithara::abr::Abr for TestPeer {}
    impl Peer for TestPeer {}
    let cancel = CancelToken::never();
    let dl = Downloader::new(
        DownloaderConfig::for_client(create_test_net())
            .cancel(cancel.child())
            .build(),
    );
    dl.register(Arc::new(TestPeer))
}

/// Build a test [`PlaylistCache`] backed by the supplied
/// [`TestAssets`] + a fresh private [`PeerHandle`].
pub fn test_playlist_cache(assets: &TestAssets, _net: HttpClient) -> PlaylistCache {
    PlaylistCache::new(
        assets.scope(),
        create_test_peer_handle(),
        kithara::bufpool::BytePool::default(),
    )
}

/// Build a test [`KeyStore`] backed by a fresh [`PeerHandle`] and
/// the supplied [`TestAssets`]. Mirrors the production constructor in
/// `Hls::create` so integration tests exercise the same wiring.
pub fn test_key_store(
    assets: &TestAssets,
    key_registry: Option<kithara::drm::KeyProcessorRegistry>,
) -> KeyStore {
    KeyStore::new(
        create_test_peer_handle(),
        assets.scope(),
        kithara::events::EventBus::new(8),
        None,
        key_registry,
        kithara::bufpool::BytePool::default(),
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
