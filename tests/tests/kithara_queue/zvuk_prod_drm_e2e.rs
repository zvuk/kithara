#![cfg(not(target_arch = "wasm32"))]

use std::{sync::Arc, time::Duration};

use kithara_app::{config::AppConfig, sources::build_source};
use kithara_assets::{FlushHub, FlushPolicy, StoreOptions};
use kithara_decode::DecoderBackend;
use kithara_events::{AbrMode, Event, EventReceiver, QueueEvent, TrackId, TrackStatus};
use kithara_integration_tests::{TestTempDir, kithara, offline::OfflineSession};
use kithara_net::{HttpClient, NetOptions};
use kithara_play::{PlayerConfig, PlayerImpl};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use tokio::{
    sync::OnceCell,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

/// Production zvuk DRM track. Server: `cdn-hls-slicer.zvuk.com`,
/// matched by the `zvuk-prod` provider in `app.yaml` (domains
/// `zvuk.com` / `*.zvuk.com`). Mirrors the URL in `app.yaml`'s
/// `playlist.tracks` so what the binary plays manually is what
/// the test plays here.
///
/// The track contains HE-AAC v2 fragments — exercise of the
/// `symphonia-adapter-fdk-aac` path for production-grade content
/// (stage DRM tracks are HE-AAC v1).
const PROD_TRACK: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8";

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
            DownloaderConfig::for_client(HttpClient::new(net, CancellationToken::new())).build(),
        );
        let flush_hub = FlushHub::new(CancellationToken::new(), FlushPolicy::default());
        let config = AppConfig::new(downloader, flush_hub, CancellationToken::new());
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .session(OfflineSession::arc_auto())
                .build(),
        ));
        let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

        let q = Arc::clone(&queue);
        tokio::spawn(async move {
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
    match build_source(url, &ctx.config) {
        TrackSource::Config(mut cfg) => {
            cfg.store = StoreOptions::new(ctx.cache.path());
            cfg.decoder_backend = backend;
            cfg.initial_abr_mode = AbrMode::Auto(None);
            TrackSource::Config(cfg)
        }
        other => other,
    }
}

async fn wait_for_loaded(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
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
                Ok(ev) => ev,
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

async fn wait_for_position_at_least(
    queue: &Queue,
    min_secs: f64,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(pos) = queue.position_seconds()
            && pos >= min_secs
        {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "position stayed below {min_secs:.2}s for {:?} (last: {:?})",
                deadline,
                queue.position_seconds()
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Production zvuk DRM end-to-end: load → select → play, asserting
/// that audio progresses. Pins the production code path the user
/// drives manually with `cargo run -p kithara-app`. Specifically
/// validates:
///
/// 1. `zvuk-prod` DRM provider in baked `app.yaml` resolves the
///    `zvuk.com` keyserver and supplies `X-Auth-Token` + `X-SP-ZV`.
/// 2. HE-AAC v2 fragments decode through `symphonia-adapter-fdk-aac`.
/// 3. `apply_commit`-via-dispatch shortcut from
///    `crates/kithara-hls/src/variant.rs` does not regress for
///    DRM-encrypted segments (PKCS7 post-decrypt size shrink).
///
/// Requires production credentials baked at build time:
///
/// ```text
/// KITHARA_DRM_PROD_KEY=... \
/// KITHARA_DRM_PROD_AUTH_TOKEN=... \
/// KITHARA_DRM_PROD_SP_ZV_TOKEN=... \
///     cargo nextest run -E 'test(zvuk_prod_drm)' --run-ignored=only
/// ```
///
/// `#[ignore]` because the upstream is VPN-gated and the creds rot.
#[kithara::test(tokio)]
#[ignore = "requires zvuk prod creds baked at build (KITHARA_DRM_PROD_*) — run with --run-ignored=only"]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn zvuk_prod_drm_track_plays(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let ctx = shared_ctx().await;
    let source = build_track_source(PROD_TRACK, ctx, backend);
    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source);

    wait_for_loaded(&mut rx, &ctx.queue, track_id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("prod DRM load fail [{PROD_TRACK}]: {e}"));

    ctx.queue
        .select(track_id, Transition::None)
        .expect("select");
    wait_for_position_at_least(&ctx.queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("prod DRM play fail [{PROD_TRACK}]: {e}"));

    let before = ctx.queue.position_seconds().unwrap_or(0.0);
    sleep(Duration::from_secs(2)).await;
    let after = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        after - before >= 0.9,
        "prod DRM playback stalled [{PROD_TRACK}]: \
         {before:.2}→{after:.2} over 2s wall clock"
    );

    ctx.queue.remove(track_id).expect("remove");
}
