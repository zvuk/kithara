#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    events::{Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep, timeout},
        tokio,
        tokio::sync::OnceCell,
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{
    config::{AppConfig, AppDrm},
    document::Config,
    pools::{AppPools, PoolsSection, build as app_pools},
    sources::build_source,
};
use kithara_integration_tests::{TestTempDir, kithara, offline::OfflineQueue};
use tracing_subscriber::EnvFilter;

/// Real-network DRM trace harness. Loads a single zvq.me DRM master
/// playlist and dumps every HLS / stream / net tracing event, so the
/// failing step is visible in nextest output (`kithara_hls=trace`,
/// `kithara_stream=trace`, `kithara_net=debug`, `kithara_app=debug`).
///
/// Lives in `suite_network` — it talks to a real VPN-gated host and is
/// pointless without `KITHARA_DRM_KEY` + `KITHARA_DRM_AUTH_TOKEN`
/// baked at build time (`option_env!`).
#[kithara::test(tokio)]
async fn zvuk_drm_master_playlist_trace() {
    install_tracing();

    let cache = TestTempDir::new();
    let ctx = shared_ctx().await;
    let url = "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8";

    let mut config = ctx.config.clone();
    config.store = super::source_helper::app_disk_asset_store(&ctx.config, cache.path());
    let source = build_source(url, &config);

    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source).expect("append DRM trace track");
    tracing::info!(%url, ?track_id, "DRM trace: track appended");

    match wait_for_terminal(&mut rx, &ctx.queue, track_id, Duration::from_secs(20)).await {
        Ok(status) => tracing::info!(?status, "DRM trace: terminal status"),
        Err(reason) => tracing::error!(reason, "DRM trace: timed out / stream closed"),
    }
}

struct Ctx {
    config: AppConfig,
    queue: OfflineQueue<AppPools>,
}

static CTX: OnceCell<Ctx> = OnceCell::const_new();

async fn shared_ctx() -> &'static Ctx {
    CTX.get_or_init(|| async {
        let pools = app_pools(&PoolsSection::default()).expect("build app pool region");
        let net = NetOptions::builder().is_insecure(true).build();
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
                .build(),
        );
        let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
        let shutdown = CancelToken::never();
        let document = Config::load(None, None).expect("the shipped configuration loads");
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

        let q = queue.control();
        tokio::task::spawn(async move {
            loop {
                sleep(Duration::from_millis(50)).await;
                let _ = q.tick();
            }
        });

        Ctx { config, queue }
    })
    .await
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "kithara_app=debug,kithara_hls=trace,kithara_stream=debug,kithara_net=debug,\
             kithara_queue=debug,kithara_drm=trace",
        )
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .with_target(true)
        .try_init();
}

async fn wait_for_terminal(
    rx: &mut EventReceiver,
    queue: &Queue<AppPools>,
    track_id: TrackId,
    deadline: Duration,
) -> Result<TrackStatus, String> {
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    if let Some(entry) = queue.track(track_id)
        && matches!(
            entry.status,
            TrackStatus::Loaded | TrackStatus::Failed(_) | TrackStatus::Consumed
        )
    {
        return Ok(entry.status);
    }
    timeout(deadline, async {
        loop {
            let ev = match rx.recv().await {
                Ok(env) => env.event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            };
            if let Event::Queue(QueueEvent::TrackStatusChanged { id, status }) = ev
                && id == track_id
            {
                tracing::info!(?status, "DRM trace: status change");
                if matches!(
                    status,
                    TrackStatus::Loaded | TrackStatus::Failed(_) | TrackStatus::Consumed
                ) {
                    return Ok(status);
                }
            }
        }
    })
    .await
    .map_err(|_| format!("no terminal status within {deadline:?}"))?
}
