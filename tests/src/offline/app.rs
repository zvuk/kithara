use std::path::PathBuf;

use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    audio::{AudioDecoderConfig, DecoderResamplerSettings},
    decode::DecoderBackend,
    download::{Downloader, DownloaderConfig},
    hls::{AbrMode, KeyOptions},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration, tokio},
    play::{
        PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend, PlayerConfig, PlayerImpl,
        ResourceSrc,
    },
    queue::{Queue, QueueConfig},
};
use kithara_app::{
    config::{AppConfig, AppDrm},
    document::Config,
    pools::{
        AppPools, AppResourceConfig, AppStore, AppTrackSource, PoolsSection, build as app_pools,
    },
};
use kithara_test_utils::TestTempDir;

use super::{OfflineQueue, QueueTicker, RENDER_PACE};

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

/// Build a product offline queue for tests that reach insecure HTTP fixtures.
pub async fn insecure_app_queue() -> AppQueueFixture {
    app_queue(Config::load(None, None).expect("the shipped configuration loads")).await
}

pub async fn app_queue(document: Config) -> AppQueueFixture {
    let pools = app_pools(&PoolsSection::default()).expect("build app pool region");
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
            .build(),
    );
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
        .drm(AppDrm::new(
            document
                .drm_policy()
                .expect("the shipped providers are valid"),
        ))
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker.clone())
        .store(store)
        .build();
    let session_config = HostConfig::offline(session_pools).build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session_config.sample_rate())
            .worker(worker)
            .build(),
    );
    let queue = OfflineQueue::paced(
        session_config,
        Queue::new(QueueConfig::builder().player(player).build()),
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
