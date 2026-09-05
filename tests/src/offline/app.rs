use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, sleep},
        tokio,
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{
    baked,
    config::AppConfig,
    pools::{AppPools, build as app_pools},
};

use super::OfflineQueue;
use crate::TestTempDir;

#[non_exhaustive]
pub struct AppQueueFixture {
    pub config: AppConfig,
    pub queue: OfflineQueue<AppPools>,
    pub cache: TestTempDir,
}

pub struct LazyAppQueueFixture(tokio::sync::OnceCell<AppQueueFixture>);

impl LazyAppQueueFixture {
    #[must_use]
    pub const fn const_new() -> Self {
        Self(tokio::sync::OnceCell::const_new())
    }

    pub async fn get(&self) -> &AppQueueFixture {
        self.0.get_or_init(|| async { insecure_app_queue() }).await
    }
}

/// Build a product offline queue for tests that reach insecure HTTP fixtures.
#[must_use]
pub fn insecure_app_queue() -> AppQueueFixture {
    let pools = app_pools().expect("build app pool region");
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
        .layouts(baked::build_baked_asset_layouts())
        .build();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools)
            .cancel(shutdown.child())
            .build(),
    );
    let session_pools = worker.pools().clone();
    let config = AppConfig::builder()
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker.clone())
        .store(store)
        .build();
    let session_config = HostConfig::offline(session_pools)
        .pacing(Duration::from_millis(10))
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session_config.sample_rate())
            .worker(worker)
            .build(),
    );
    let queue = OfflineQueue::new(
        session_config,
        Queue::new(QueueConfig::builder().player(player).build()),
    )
    .expect("create product offline queue");

    let queue_for_tick = queue.control();
    tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            let _ = queue_for_tick.tick();
        }
    });

    AppQueueFixture {
        config,
        queue,
        cache: TestTempDir::new(),
    }
}
