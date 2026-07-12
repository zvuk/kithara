#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{FlushHub, FlushPolicy, StoreOptions},
    bufpool::{BytePool, PcmPool},
    events::{Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep, timeout},
        tokio,
        tokio::sync::OnceCell,
    },
    play::{PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig, TrackSource},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{config::AppConfig, sources::build_source};
use kithara_integration_tests::{TestTempDir, kithara};
use tracing_subscriber::EnvFilter;

/// Real-network DRM trace harness. Loads a single zvq.me DRM master
/// playlist and dumps every HLS / stream / net tracing event, so the
/// failing step is visible in nextest output (`kithara_hls=trace`,
/// `kithara_stream=trace`, `kithara_net=debug`, `kithara_app=debug`).
///
/// Marked `#[ignore]` — it talks to a real VPN-gated host and is
/// pointless without `KITHARA_DRM_KEY` + `KITHARA_DRM_AUTH_TOKEN`
/// baked at build time (`option_env!`).
#[kithara::test(tokio)]
#[ignore = "requires zvq.me VPN + .env-baked DRM creds — `just zvuk-drm-trace`"]
async fn zvuk_drm_master_playlist_trace() {
    install_tracing();

    let cache = TestTempDir::new();
    let ctx = shared_ctx().await;
    let url = "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8";

    let source = match build_source(url, &ctx.config) {
        TrackSource::Config(mut cfg) => {
            cfg.store = StoreOptions::new(cache.path());
            TrackSource::Config(cfg)
        }
        other => other,
    };

    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source);
    tracing::info!(%url, ?track_id, "DRM trace: track appended");

    match wait_for_terminal(&mut rx, &ctx.queue, track_id, Duration::from_secs(20)).await {
        Ok(status) => tracing::info!(?status, "DRM trace: terminal status"),
        Err(reason) => tracing::error!(reason, "DRM trace: timed out / stream closed"),
    }
}

struct Ctx {
    config: AppConfig,
    queue: Arc<Queue>,
}

static CTX: OnceCell<Ctx> = OnceCell::const_new();

async fn shared_ctx() -> &'static Ctx {
    CTX.get_or_init(|| async {
        let net = NetOptions::builder().is_insecure(true).build();
        let downloader = Downloader::new(
            DownloaderConfig::builder()
                .client(HttpClient::new(net, CancelToken::never()))
                .build(),
        );
        let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
        let config = AppConfig::new(
            downloader,
            flush_hub,
            CancelToken::never(),
            BytePool::default(),
            PcmPool::default(),
        );
        let player = Arc::new(PlayerImpl::new(PlayerConfig::builder().build()));
        let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

        let q = Arc::clone(&queue);
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
    queue: &Queue,
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
                Ok(ev) => ev,
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
