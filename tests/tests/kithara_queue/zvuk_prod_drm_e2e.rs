#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    decode::DecoderBackend,
    events::{AbrMode, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
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
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{
    config::{AppConfig, AppDrm},
    document::Config,
    pools::{AppPools, PoolsSection, build as app_pools},
};
use kithara_integration_tests::{
    TestTempDir, kithara, offline::OfflineQueue, waits::wait_for_position_at_least,
};

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
    queue: OfflineQueue<AppPools>,
    cache: TestTempDir,
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

        Ctx {
            config,
            queue,
            cache: TestTempDir::new(),
        }
    })
    .await
}

fn build_track_source(url: &str, ctx: &Ctx, backend: DecoderBackend) -> TrackSource<AppPools> {
    super::app_track_source(
        url,
        &ctx.config,
        super::app_disk_asset_store(&ctx.config, ctx.cache.path()),
        backend,
        AbrMode::Auto(None),
        None,
    )
}

async fn wait_for_loaded(
    rx: &mut EventReceiver,
    queue: &QueueControl<AppPools>,
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
///     just test run --lane=network -E 'test(zvuk_prod_drm)'
/// ```
///
/// Lives in `suite_network` because the upstream is VPN-gated and the creds
/// rot.
#[kithara::test(tokio)]
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
    let track_id = ctx
        .queue
        .append(source)
        .expect("append production DRM track");

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
    wait_for_position_at_least(&ctx.queue, before + 0.9, Duration::from_secs(5))
        .await
        .unwrap_or_else(|e| {
            panic!("prod DRM playback stalled [{PROD_TRACK}]: did not advance ≥0.9s from {before:.2}: {e}")
        });
    let after = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        after - before >= 0.9,
        "prod DRM playback stalled [{PROD_TRACK}]: \
         {before:.2}→{after:.2} (advance below 0.9s)"
    );

    ctx.queue.remove(track_id).expect("remove");
}
