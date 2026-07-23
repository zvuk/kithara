#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{FlushHub, FlushPolicy},
    bufpool::{BytePool, PcmPool},
    decode::DecoderBackend,
    events::{AbrMode, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep, timeout},
        tokio,
        tokio::sync::OnceCell,
    },
    play::{PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::config::AppConfig;
use kithara_integration_tests::{
    TestTempDir, kithara, offline::OfflineSession, waits::wait_for_position_at_least,
};

/// Staging zvq.me DRM track — `zvuk-stage` provider in `app.yaml`.
/// Validates the per-provider X-Encrypted-Key salt shape: stage WAF
/// requires 16-char alphanumeric (legacy zvqengine format captured
/// in `zvuk_cipher_check.rs::STAGE_PLAINTEXT`), not the iOS
/// `randomString(of: 8)` hex format used by prod.
const STAGE_TRACK: &str = "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8";

struct Ctx {
    config: AppConfig,
    queue: Arc<Queue>,
    cache: TestTempDir,
}

static CTX: OnceCell<Ctx> = OnceCell::const_new();

async fn shared_ctx() -> &'static Ctx {
    CTX.get_or_init(|| async {
        let net = NetOptions::builder().is_insecure(true).build();
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(net, CancelToken::never())).build(),
        );
        let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
        let config = AppConfig::new(
            downloader,
            flush_hub,
            CancelToken::never(),
            BytePool::default(),
            PcmPool::default(),
        );
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .session(OfflineSession::arc_auto())
                .build(),
        ));
        let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

        let q = Arc::clone(&queue);
        tokio::task::spawn(async move {
            loop {
                sleep(Duration::from_millis(50)).await;
                let _ = q.tick();
            }
        });

        Ctx {
            config,
            queue,
            cache: TestTempDir::new(),
        }
    })
    .await
}

fn build_track_source(url: &str, ctx: &Ctx, backend: DecoderBackend) -> TrackSource {
    super::app_track_source(
        url,
        &ctx.config,
        kithara_integration_tests::disk_asset_store(ctx.cache.path()),
        backend,
        AbrMode::Auto(None),
        None,
    )
}

async fn wait_for_loaded(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    if let Some(entry) = queue.track(track_id) {
        match &entry.status {
            TrackStatus::Loaded => return Ok(()),
            TrackStatus::Failed(err) => return Err(format!("Failed before subscribe: {err}")),
            _ => {}
        }
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
                match status {
                    TrackStatus::Loaded => return Ok(()),
                    TrackStatus::Failed(err) => return Err(format!("Failed: {err}")),
                    _ => continue,
                }
            }
        }
    })
    .await
    .map_err(|_| format!("no Loaded within {deadline:?}"))?
}

/// Staging zvq.me DRM end-to-end: load → select → play, asserting
/// that audio progresses (position advances by ≥0.9s over 2s wall
/// clock). Mirrors `zvuk_prod_drm_e2e::zvuk_prod_drm_track_plays`
/// but exercises the `zvuk-stage` provider with legacy 16-char
/// alphanumeric salt.
///
/// Requires staging credentials baked at build time:
///
/// ```text
/// KITHARA_DRM_STAGE_KEY=BinaryCipherKey \
/// KITHARA_DRM_STAGE_AUTH_TOKEN=... \
///     cargo nextest run -E 'test(zvuk_stage_drm)' --run-ignored=only
/// ```
#[kithara::test(tokio)]
#[ignore = "PARKED 2026-05-20: stage keyserver returns keys that don't decrypt their \
            segments (3/3 tracks tested); waiting on server-team. Re-enable when \
            stage DRM confirmed working — needs KITHARA_DRM_STAGE_* creds + VPN."]
#[case::symphonia(DecoderBackend::Symphonia)]
async fn zvuk_stage_drm_track_plays(#[case] backend: DecoderBackend) {
    let ctx = shared_ctx().await;
    let source = build_track_source(STAGE_TRACK, ctx, backend);
    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source);

    wait_for_loaded(&mut rx, &ctx.queue, track_id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("stage DRM load fail [{STAGE_TRACK}]: {e}"));

    ctx.queue
        .select(track_id, Transition::None)
        .expect("select");
    wait_for_position_at_least(&ctx.queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("stage DRM play fail [{STAGE_TRACK}]: {e}"));

    let before = ctx.queue.position_seconds().unwrap_or(0.0);
    wait_for_position_at_least(&ctx.queue, before + 0.9, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("stage DRM playback stalled [{STAGE_TRACK}]: {e}"));
    let after = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        after - before >= 0.9,
        "stage DRM playback stalled [{STAGE_TRACK}]: \
         {before:.2}→{after:.2} (waited on position advance)"
    );

    ctx.queue.remove(track_id).expect("remove");
}
