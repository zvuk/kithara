use std::{num::NonZeroUsize, path::PathBuf};

use kithara::{
    analysis::BeatAnalysisConfigPatchError,
    assets::{FlushHub, FlushPolicy},
    bufpool::PoolError,
    download::{Downloader, DownloaderConfig},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, thread, tokio::runtime::Handle},
    play::PlayWorkerConfig,
    worker::{OwnedPoolConfig, Worker, WorkerConfig},
};

use super::{Config, PolicyError};
use crate::{
    config::{AppBroadcastConfig, AppConfig, AppDrm},
    pools::{AppStore, AppWorker, Pools},
};

/// Why a document could not be assembled into an [`AppConfig`].
#[derive(Debug, derive_more::Display, derive_more::Error, derive_more::From)]
#[non_exhaustive]
pub enum AssembleError {
    /// The `drm:` section declares a policy that cannot be honoured.
    #[display("{_0}")]
    Drm(PolicyError),
    /// The `beat:` section was refused.
    #[display("beat: {_0}")]
    Beat(BeatAnalysisConfigPatchError),
    /// The `draw_pool:` section fails the draw-buffer schema.
    #[display("draw_pool: {_0}")]
    DrawPool(PoolError),
}

#[bon::bon]
impl AppConfig {
    /// Assembles what `document` describes over `pools`.
    ///
    /// # Errors
    /// Returns [`AssembleError`] when a section of the document is refused.
    #[builder]
    pub fn assemble(
        document: &Config,
        pools: Pools,
        shutdown: CancelToken,
        runtime: Handle,
        #[builder(default)] is_insecure: bool,
        ui_package: Option<PathBuf>,
    ) -> Result<Self, AssembleError> {
        let compute_threads = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
        let mut worker_config = WorkerConfig::new()
            .with_cancel(shutdown.child())
            .with_runtime(runtime)
            .with_max_compute_tasks(compute_threads)
            .with_owned_pool(OwnedPoolConfig::new(compute_threads, "kithara-compute"));
        worker_config.apply(document.worker());
        let base_worker = Worker::new(worker_config);
        let mut play_worker_config = PlayWorkerConfig::builder(pools.clone())
            .cancel(shutdown.child())
            .worker(base_worker.clone())
            .build();
        play_worker_config.apply(document.play_worker());
        let worker = AppWorker::new(play_worker_config);
        #[cfg(feature = "broadcast")]
        let broadcast = {
            let mut broadcast = AppBroadcastConfig::builder(base_worker.clone(), pools.clone())
                .cancel(shutdown.child())
                .build();
            broadcast.apply(document.broadcast());
            broadcast
        };
        #[cfg(not(feature = "broadcast"))]
        let broadcast = AppBroadcastConfig::default();
        let mut net = NetOptions::builder().build();
        net.apply(document.net());
        net.is_insecure |= is_insecure;
        let should_accept_invalid_certs = net.is_insecure;
        let mut downloader_config =
            DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), shutdown.child()))
                .build();
        downloader_config.apply(document.downloader());
        let downloader = Downloader::new(downloader_config);
        let mut flush_policy = FlushPolicy::default();
        flush_policy.apply(document.flush());
        let flush_hub = FlushHub::new(shutdown.child(), flush_policy);
        let mut store_config = AppStore::builder(pools)
            .cancel(shutdown.child())
            .flush_hub(flush_hub)
            .layouts(document.asset_layouts())
            .into_config();
        store_config.apply(document.assets_store());
        let store = AppStore::open(store_config);
        let builder = Self::builder()
            .drm(AppDrm::new(document.drm_policy()?))
            .beat_analysis(document.beat()?)
            .downloader(downloader)
            .shutdown(shutdown)
            .worker(worker)
            .base_worker(base_worker)
            .broadcast(broadcast)
            .store(store)
            .queue(document.queue())
            .dispatcher(document.dispatcher())
            .player(document.player())
            .audio(document.audio())
            .hls(document.hls())
            .file(document.file());
        #[cfg(feature = "gui")]
        let builder = builder.ui(document.ui()?);
        let mut config = builder
            .tracks(document.tracks().to_vec())
            .should_accept_invalid_certs(should_accept_invalid_certs)
            .maybe_ui_package(ui_package)
            .build();
        config.apply(document.app());
        Ok(config)
    }
}
