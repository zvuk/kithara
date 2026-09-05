#[cfg(not(target_arch = "wasm32"))]
use kithara::assets::StorageBackend;
use kithara::{
    assets::{
        AssetLayout, AssetLayoutRegistry, AssetResource, AssetScope, AssetSource, AssetStore,
        DefaultLayout,
    },
    hls::{Hls, KeyStore, PlaylistCache},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc},
    stream::dl::{Downloader, DownloaderConfig, Peer, PeerHandle},
};
use url::Url;

use crate::{
    TestTempDir,
    bufpool_ext::{Pools, TestPools, pools},
};

/// Wrapper for test assets with temp directory lifetime management
pub struct TestAssets {
    assets: AssetStore<TestPools>,
    pools: Pools,
    source: AssetSource,
    #[cfg(not(target_arch = "wasm32"))]
    _temp_dir: Arc<TestTempDir>,
}

#[derive(Debug)]
struct TestHlsLayout;

impl AssetLayout for TestHlsLayout {
    fn root(&self, source: &AssetSource) -> String {
        let AssetSource::Remote {
            discriminator: Some(root),
            ..
        } = source
        else {
            panic!("test HLS source must carry its literal root")
        };
        root.clone()
    }

    fn path(&self, resource: &AssetResource) -> String {
        DefaultLayout.path(resource)
    }
}

fn test_source(asset_root: &str) -> AssetSource {
    AssetSource::Remote {
        url: Url::parse("https://cache.test/master.m3u8").expect("valid test URL"),
        discriminator: Some(asset_root.to_string()),
    }
}

fn test_layouts() -> AssetLayoutRegistry {
    AssetLayoutRegistry::default().with::<Hls<TestPools>>(Arc::new(TestHlsLayout))
}

impl TestAssets {
    pub const fn assets(&self) -> &AssetStore<TestPools> {
        &self.assets
    }

    pub const fn pools(&self) -> &Pools {
        &self.pools
    }

    /// Scope bound to this fixture's `asset_root`, mirroring how
    /// `Hls::create` scopes its per-stream store.
    pub fn scope(&self) -> AssetScope<TestPools> {
        self.assets
            .scope::<Hls<TestPools>>(&self.source)
            .expect("valid test HLS source")
    }
}

/// Create test assets with default "test-hls" root
pub fn create_test_assets() -> TestAssets {
    create_test_assets_with_root("test-hls")
}

/// Create test assets with custom asset root.
pub fn create_test_assets_with_root(asset_root: &str) -> TestAssets {
    let pools = pools();
    let builder = AssetStore::builder(pools.clone());

    #[cfg(not(target_arch = "wasm32"))]
    let (builder, temp_dir) = {
        let temp_dir = Arc::new(TestTempDir::new());
        let builder = builder.backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        });
        (builder, temp_dir)
    };

    let assets = builder
        .cancel(CancelToken::never())
        .layouts(test_layouts())
        .build();

    TestAssets {
        assets,
        pools,
        source: test_source(asset_root),
        #[cfg(not(target_arch = "wasm32"))]
        _temp_dir: temp_dir,
    }
}

/// Create test HTTP client with default options
pub fn create_test_net() -> HttpClient {
    HttpClient::new(NetOptions::default(), pools(), CancelToken::never())
}

/// Create a private test [`Downloader`] with a fresh cancel token.
pub fn create_test_downloader() -> Downloader {
    Downloader::new(DownloaderConfig::for_client(create_test_net()).build())
}

/// Create a private test [`PeerHandle`] via `Downloader::register`.
fn create_test_peer_handle(pools: &Pools) -> PeerHandle {
    struct TestPeer {
        cancel: CancelToken,
    }
    impl kithara::abr::Abr for TestPeer {
        fn cancel(&self) -> CancelToken {
            self.cancel.clone()
        }
    }
    impl Peer for TestPeer {}
    let cancel = CancelToken::never();
    let client = HttpClient::new(NetOptions::default(), pools.clone(), CancelToken::never());
    let dl = Downloader::new(
        DownloaderConfig::for_client(client)
            .cancel(cancel.child())
            .build(),
    );
    dl.register(Arc::new(TestPeer {
        cancel: cancel.clone(),
    }))
}

/// Build a test [`PlaylistCache`] backed by the supplied
/// [`TestAssets`] + a fresh private [`PeerHandle`].
pub fn test_playlist_cache(assets: &TestAssets, _net: HttpClient) -> PlaylistCache<TestPools> {
    PlaylistCache::new(
        assets.scope(),
        create_test_peer_handle(assets.pools()),
        assets.pools().clone(),
    )
}

/// Build a test [`KeyStore`] backed by a fresh [`PeerHandle`] and
/// the supplied [`TestAssets`]. Mirrors the production constructor in
/// `Hls::create` so integration tests exercise the same wiring.
pub fn test_key_store(
    assets: &TestAssets,
    key_registry: Option<kithara::drm::KeyProcessorRegistry>,
) -> KeyStore<TestPools> {
    KeyStore::new(
        create_test_peer_handle(assets.pools()),
        assets.scope(),
        kithara::events::EventBus::new(8),
        None,
        key_registry,
        assets.pools().clone(),
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
