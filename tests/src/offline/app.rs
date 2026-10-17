use std::path::PathBuf;

use keyring_core::sample::Store;
use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    audio::{AudioDecoderConfig, DecoderResamplerSettings},
    decode::DecoderBackend,
    download::{Downloader, DownloaderConfig},
    hls::{AbrMode, KeyOptions},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration, tokio},
    play::{PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend, ResourcePrep, ResourceSrc},
    queue::{Queue, QueueConfig},
};
use kithara_app::{
    config::{AppConfig, AppDrm},
    document::Config,
    plugins,
    pools::{
        AppPools, AppResourceConfig, AppStore, AppTrackSource, PoolsSection, build as app_pools,
    },
    secret,
};
use kithara_app_library::{Environment, Secrets};
use kithara_test_utils::TestTempDir;

use super::{OfflineQueue, QueueTicker, RENDER_PACE};

/// The DRM policy the binary builds: the plugins `document` configures
/// register over `client` with the Zvuk account token from
/// `KITHARA_DRM_PROD_AUTH_TOKEN`, when set, in their secret store, and the key
/// access they grant reaches the key requests.
#[must_use]
pub fn app_drm(document: &Config, client: &HttpClient, shutdown: &CancelToken) -> AppDrm {
    let secrets = Secrets::new(Store::new());
    if let Some(token) = secret("KITHARA_DRM_PROD_AUTH_TOKEN") {
        secrets.set("zvuk", &token).expect("the store writes");
    }
    let environment = Environment::new(tokio::runtime::Handle::current(), client.clone(), secrets);
    let registered =
        plugins::mount(document, &environment, shutdown).expect("the configured plugins register");
    AppDrm::new(
        document
            .drm_policy(&plugins::grants(&registered))
            .expect("the shipped providers are valid"),
    )
}

#[non_exhaustive]
pub struct AppQueueFixture {
    pub config: AppConfig,
    pub queue: OfflineQueue<AppPools>,
    pub cache: TestTempDir,
    ticker: QueueTicker,
}

impl AppQueueFixture {
    pub async fn close(self) {
        let Self {
            config,
            queue,
            cache,
            mut ticker,
        } = self;
        ticker.stop().await;
        drop(config);
        queue.close().await;
        drop(cache);
    }
}

pub struct LazyAppQueueFixture(tokio::sync::OnceCell<AppQueueFixture>);

impl LazyAppQueueFixture {
    #[must_use]
    pub const fn const_new() -> Self {
        Self(tokio::sync::OnceCell::const_new())
    }

    pub async fn get(&self) -> &AppQueueFixture {
        self.0.get_or_init(insecure_app_queue).await
    }
}

/// The shipped document, which mounts the Zvuk source.
#[must_use]
pub fn prod_document() -> Config {
    Config::load(None, None).expect("the production configuration loads")
}

/// Build a product offline queue for tests that reach insecure HTTP fixtures.
pub async fn insecure_app_queue() -> AppQueueFixture {
    app_queue(prod_document()).await
}

pub async fn app_queue(document: Config) -> AppQueueFixture {
    let pools = app_pools(&PoolsSection::default()).expect("build app pool region");
    let net = NetOptions::builder().is_insecure(true).build();
    let client = HttpClient::new(net, pools.clone(), CancelToken::never());
    let downloader = Downloader::new(DownloaderConfig::for_client(client.clone()).build());
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let shutdown = CancelToken::never();
    let store = AssetStore::builder(pools.clone())
        .cancel(shutdown.child())
        .backend(StorageBackend::default())
        .flush_hub(flush_hub)
        .layouts(document.asset_layouts())
        .build();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools)
            .cancel(shutdown.child())
            .build(),
    );
    let session_pools = worker.pools().clone();
    let config = AppConfig::builder()
        .drm(app_drm(&document, &client, &shutdown))
        .net(client)
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker.clone())
        .store(store)
        .build();
    let session_config = HostConfig::offline(session_pools).build();
    let prep = ResourcePrep::builder().worker(worker).build();
    let queue = OfflineQueue::paced(
        session_config,
        Queue::new(
            QueueConfig::builder()
                .prep(prep)
                .store(config.store.clone())
                .build(),
        ),
        RENDER_PACE,
    )
    .await
    .expect("create product offline queue");

    let queue_for_tick = queue.control();
    let ticker = QueueTicker::spawn(queue_for_tick, Duration::from_millis(50));

    AppQueueFixture {
        config,
        queue,
        cache: TestTempDir::new(),
        ticker,
    }
}

/// A disk-backed asset store under `root`, built from the app pools.
pub fn app_disk_asset_store(config: &AppConfig, root: impl Into<PathBuf>) -> AppStore {
    AssetStore::builder(config.worker.pools().clone())
        .backend(StorageBackend::Disk { root: root.into() })
        .build()
}

/// A track source configured the way the app configures one: its DRM keys and
/// headers, downloader, worker, decoder and store.
pub fn app_track_source(
    url: &str,
    config: &AppConfig,
    store: AppStore,
    backend: DecoderBackend,
    abr: AbrMode,
    discriminator: Option<&str>,
) -> AppTrackSource {
    let Ok(src) = ResourceSrc::parse(url) else {
        return AppTrackSource::Uri(url.to_string());
    };
    let builder = AppResourceConfig::for_src(src);
    let registry = config.drm.registry();
    let keys = if registry.is_empty() {
        KeyOptions::default()
    } else {
        KeyOptions::builder().key_registry(registry.clone()).build()
    };
    let headers = url::Url::parse(url)
        .ok()
        .and_then(|parsed| config.drm.resource_headers(&parsed));
    let decoder_defaults = AudioDecoderConfig::builder()
        .resampler(
            DecoderResamplerSettings::builder()
                .backend(PlaybackResamplerBackend::default())
                .build(),
        )
        .build();
    let decoder = AudioDecoderConfig::builder()
        .backend(backend)
        .gapless_mode(decoder_defaults.gapless_mode())
        .maybe_resampler(decoder_defaults.resampler().cloned())
        .build();
    let builder = builder
        .downloader(config.downloader.clone())
        .worker(config.worker.clone())
        .keys(keys)
        .maybe_headers(headers)
        .audio(config.audio.clone())
        .maybe_preload_chunks(config.preload_chunks)
        .maybe_audio_buffer_chunks(config.audio_buffer_chunks)
        .hls(config.hls.clone())
        .file(config.file.clone())
        .store(store)
        .decoder(decoder)
        .initial_abr_mode(abr);
    let config = match discriminator {
        Some(discriminator) => builder.discriminator(discriminator).build(),
        None => builder.build(),
    };
    AppTrackSource::Config(Box::new(config))
}
