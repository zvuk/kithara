use arc_swap::ArcSwap;
use kithara::{
    assets::StorageBackend,
    download::{Downloader, DownloaderConfig},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc, tokio::sync::mpsc::UnboundedSender},
    play::{PlayWorkerConfig, policy::DomainKeyPolicy},
};

use super::frontend::Boot;
use crate::{
    config::{AppConfig, AppDrm},
    engine::{EngineSnapshot, Envelope},
    pools::{self, AppStore, AppWorker, PoolsSection},
};

pub(super) fn config() -> AppConfig {
    let shutdown = CancelToken::root();
    let pools = pools::build(&PoolsSection::default()).expect("valid app pool policy");
    let worker = AppWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::builder().build(),
            pools.clone(),
            shutdown.child(),
        ))
        .build(),
    );
    let store = AppStore::builder(pools)
        .backend(StorageBackend::Memory)
        .build();
    AppConfig::builder()
        .drm(AppDrm::new(DomainKeyPolicy::new(Vec::new())))
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker)
        .store(store)
        .build()
}

pub(super) fn boot(
    config: &AppConfig,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
    commands: UnboundedSender<Envelope>,
) -> Boot {
    Boot::builder()
        .settings(&config.ui)
        .tracks(vec![
            "/music/local.flac".to_string(),
            "https://example.test/stream.m3u8".to_string(),
        ])
        .palette(config.palette)
        .snapshots(snapshots)
        .commands(commands)
        .build()
        .expect("shipped UI compiles")
}
