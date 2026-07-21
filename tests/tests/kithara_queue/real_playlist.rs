#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{FlushHub, FlushPolicy},
    bufpool::{BytePool, PcmPool},
    decode::DecoderBackend,
    events::{AbrMode, AdvanceReason, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep, timeout},
        tokio,
        tokio::sync::OnceCell,
        traits::FromWithParams,
    },
    play::{PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{baked, config::AppConfig};
use kithara_integration_tests::{
    TestTempDir, Xorshift64, kithara,
    offline::OfflineSession,
    waits::{wait_for_position_at_least, wait_for_position_near},
};

#[path = "source_helper.rs"]
mod source_helper;

/// Per-process singleton: one offline audio session, one Downloader,
/// one Queue. `#[case]` tests inside this file share it, so init cost
/// (network TLS context, audio graph) is paid once.
struct TestCtx {
    config: AppConfig,
    queue: Arc<Queue>,
    /// Isolated cache dir, shared by every track this test binary
    /// loads. Auto-deletes when the process exits, so real-network
    /// runs don't pollute the shared app cache at
    /// `env::temp_dir()/kithara`.
    cache: TestTempDir,
}

/// Same as [`build_source`] but overrides `store.cache_dir` with this
/// process's private temp dir so the real `kithara-app` cache stays
/// clean.
fn build_track_source(
    url: &str,
    ctx: &TestCtx,
    backend: DecoderBackend,
    abr: AbrMode,
) -> TrackSource {
    source_helper::app_track_source(
        url,
        &ctx.config,
        kithara_integration_tests::disk_asset_store(ctx.cache.path()),
        backend,
        abr,
        None,
    )
}

mod test_statics {
    use super::*;
    pub(super) static TEST_CTX: OnceCell<TestCtx> = OnceCell::const_new();
}

async fn shared_test_ctx() -> &'static TestCtx {
    test_statics::TEST_CTX
        .get_or_init(|| async {
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
            let player = Arc::new(PlayerImpl::new(
                PlayerConfig::builder()
                    .byte_pool(BytePool::default())
                    .pcm_pool(PcmPool::default())
                    .session(OfflineSession::arc_auto())
                    .build(),
            ));
            let queue = Arc::new(Queue::build(player, QueueConfig::default()));

            let queue_for_tick = Arc::clone(&queue);
            tokio::task::spawn(async move {
                loop {
                    sleep(Duration::from_millis(50)).await;
                    let _ = queue_for_tick.tick();
                }
            });

            TestCtx {
                config,
                queue,
                cache: TestTempDir::new(),
            }
        })
        .await
}

async fn wait_for_status(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    target: TrackStatus,
    deadline: Duration,
) -> Result<(), String> {
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    if let Some(entry) = queue.track(track_id) {
        if entry.status == target {
            return Ok(());
        }
        if let TrackStatus::Failed(err) = &entry.status {
            return Err(format!("track entered Failed: {err}"));
        }
    }
    let res = timeout(deadline, async {
        loop {
            let ev = match rx.recv().await {
                Ok(env) => env.event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            };
            if let Event::Queue(QueueEvent::TrackStatusChanged { id, status }) = ev
                && id == track_id
            {
                match &status {
                    s if *s == target => return Ok(()),
                    TrackStatus::Failed(err) => {
                        return Err(format!("track entered Failed: {err}"));
                    }
                    _ => continue,
                }
            }
        }
    })
    .await;
    match res {
        Ok(r) => r,
        Err(_) => Err(format!(
            "timeout waiting for {target:?} after {:?}",
            deadline
        )),
    }
}

async fn sample_positions(queue: &Queue, count: usize, interval: Duration) -> Vec<f64> {
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(queue.position_seconds().unwrap_or(0.0));
        sleep(interval).await;
    }
    out
}

fn assert_monotonic_nondecreasing(samples: &[f64], url: &str) {
    for w in samples.windows(2) {
        assert!(
            w[1] >= w[0] - 0.05,
            "position regressed on [{url}]: {samples:?}"
        );
    }
}

/// For each URL in the production playlist: load → play → seek ×3
/// random → position consistency. Isolates track-specific regressions
/// (DRM 403, MP3 seek-near-end hang, position drift).
// flash(false): real-CDN e2e; sleeps are wall-clock gain windows racing real sockets.
#[kithara::test(tokio)]
#[ignore = "requires silvercomet.top real network — run with --include-ignored"]
#[case::silvercomet_mp3_symphonia(
    "https://stream.silvercomet.top/track.mp3",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_mp3_apple(
        "https://stream.silvercomet.top/track.mp3",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::silvercomet_mp3_android(
        "https://stream.silvercomet.top/track.mp3",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::silvercomet_hls_symphonia_auto(
    "https://stream.silvercomet.top/hls/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[case::silvercomet_hls_symphonia_locked_low(
    "https://stream.silvercomet.top/hls/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::manual(0)
)]
#[case::silvercomet_hls_symphonia_locked_high(
    "https://stream.silvercomet.top/hls/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::manual(2)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_hls_apple_auto(
        "https://stream.silvercomet.top/hls/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_hls_apple_locked_low(
        "https://stream.silvercomet.top/hls/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::manual(0)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_hls_apple_locked_high(
        "https://stream.silvercomet.top/hls/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::manual(2)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::silvercomet_hls_android(
        "https://stream.silvercomet.top/hls/master.m3u8",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::silvercomet_drm_symphonia_auto(
    "https://stream.silvercomet.top/drm/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[case::silvercomet_drm_symphonia_locked_low(
    "https://stream.silvercomet.top/drm/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::manual(0)
)]
#[case::silvercomet_drm_symphonia_locked_high(
    "https://stream.silvercomet.top/drm/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::manual(2)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_drm_apple_auto(
        "https://stream.silvercomet.top/drm/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_drm_apple_locked_low(
        "https://stream.silvercomet.top/drm/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::manual(0)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_drm_apple_locked_high(
        "https://stream.silvercomet.top/drm/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::manual(2)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::silvercomet_drm_android(
        "https://stream.silvercomet.top/drm/master.m3u8",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_hq_1_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_hq_1_apple(
        "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_hq_1_android(
        "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_hq_2_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_hq_2_apple(
        "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_hq_2_android(
        "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_hq_3_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_hq_3_apple(
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_hq_3_android(
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_drm_1_symphonia(
    "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_drm_1_apple(
        "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_drm_1_android(
        "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_hls_1_symphonia(
    "https://ecs-stage-slicer-01.zvq.me/hls/track/176000075_1/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_hls_1_apple(
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000075_1/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_hls_1_android(
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000075_1/master.m3u8",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_drm_2_symphonia(
    "https://ecs-stage-slicer-01.zvq.me/drm/track/176000094_1/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_drm_2_apple(
        "https://ecs-stage-slicer-01.zvq.me/drm/track/176000094_1/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_drm_2_android(
        "https://ecs-stage-slicer-01.zvq.me/drm/track/176000094_1/master.m3u8",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_hls_2_symphonia(
    "https://ecs-stage-slicer-01.zvq.me/hls/track/176000109_1/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_hls_2_apple(
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000109_1/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_hls_2_android(
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000109_1/master.m3u8",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_prod_drm_flac_apple(
        "https://cdn-hls-slicer.zvuk.com/drm/track/125895892_2/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::manual(3)
    )
)]
async fn track_plays_end_to_end(
    #[case] url: &str,
    #[case] rng_seed: u64,
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let ctx = shared_test_ctx().await;
    let source = build_track_source(url, ctx, backend, abr);
    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source);

    wait_for_status(
        &mut rx,
        &ctx.queue,
        track_id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load fail [{url}]: {e}"));

    ctx.queue
        .select(track_id, Transition::None)
        .expect("select");
    wait_for_position_at_least(&ctx.queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("play fail [{url}]: {e}"));
    let progress = sample_positions(&ctx.queue, 5, Duration::from_millis(200)).await;
    assert_monotonic_nondecreasing(&progress, url);

    let duration = ctx
        .queue
        .duration_seconds()
        .expect("duration known after Loaded");
    let mut rng = Xorshift64::new(rng_seed);
    for i in 0..3 {
        let target = duration * rng.range_f64(0.05, 0.95);
        ctx.queue.seek(target).expect("seek");
        wait_for_position_near(&ctx.queue, target, 1.0, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("seek #{i} to {target:.1}s fail [{url}]: {e}"));
        let before = ctx.queue.position_seconds().unwrap_or(0.0);
        wait_for_position_at_least(&ctx.queue, before + 0.5, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("seek #{i} hang [{url}]: {e}"));
        let after = ctx.queue.position_seconds().unwrap_or(0.0);
        assert!(
            after - before >= 0.5,
            "seek #{i} hang [{url}]: {before:.2}→{after:.2}"
        );
    }

    let start_pos = ctx.queue.position_seconds().unwrap_or(0.0);
    time::sleep(Duration::from_secs(2)).await;
    let end_pos = ctx.queue.position_seconds().unwrap_or(0.0);
    let gain = end_pos - start_pos;
    assert!(
        (0.9..=2.5).contains(&gain),
        "position gain out of offline-realtime window [{url}]: got \
         {gain:.2}s over 2s wall clock (expected 0.9..2.5; start=\
         {start_pos:.2} end={end_pos:.2})",
    );

    ctx.queue.remove(track_id).expect("remove");
}

async fn wait_for_queue_event<F>(
    rx: &mut EventReceiver,
    mut pred: F,
    deadline: Duration,
) -> Option<QueueEvent>
where
    F: FnMut(&QueueEvent) -> bool,
{
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    let res = timeout(deadline, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::Queue(ev)) if pred(&ev) => return Some(ev),
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    })
    .await;
    res.unwrap_or(None)
}

/// Drive `AppConfig::DEFAULT_TRACKS` (all 10 URLs including DRM) end-
/// to-end: play first, pause/resume, seek, manual crossfade, auto-
/// advance through the rest, `QueueEnded` on the last. Per-track
/// failures are collected and reported in a structured final panic
/// so DRM regressions surface as a list instead of killing the whole
/// test at the first bad entry.
// flash(false): real-CDN e2e; sleeps are wall-clock pause/gain windows racing real sockets.
#[kithara::test(tokio)]
#[ignore = "requires AppConfig::DEFAULT_TRACKS real-network URLs (incl. silvercomet + DRM) — run with --include-ignored"]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn queue_playlist_behavior(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let ctx = shared_test_ctx().await;
    let urls: Vec<&'static str> = baked::BAKED_TRACKS.to_vec();
    assert!(urls.len() >= 3, "need ≥3 tracks for scenario");

    ctx.queue.set_crossfade_duration(2.0);

    let mut rx = ctx.queue.subscribe();
    let ids: Vec<TrackId> = urls
        .iter()
        .map(|u| {
            ctx.queue
                .append(build_track_source(u, ctx, backend, AbrMode::Auto(None)))
        })
        .collect();

    ctx.queue
        .select(ids[0], Transition::None)
        .expect("select first");
    wait_for_status(
        &mut rx,
        &ctx.queue,
        ids[0],
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("first track load [{}]: {e}", urls[0]));
    wait_for_position_at_least(&ctx.queue, 2.0, Duration::from_secs(15))
        .await
        .expect("first track position");

    let before_pause = ctx.queue.position_seconds().unwrap_or(0.0);
    ctx.queue.pause();
    time::sleep(Duration::from_secs(2)).await;
    let during_pause = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        (during_pause - before_pause).abs() < 0.5,
        "position drifted during pause: {before_pause:.2} → {during_pause:.2}"
    );
    ctx.queue.play();
    wait_for_position_at_least(&ctx.queue, during_pause + 0.01, Duration::from_secs(5))
        .await
        .expect("resume did not advance position");
    let after_resume = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        after_resume >= during_pause - 0.1,
        "resume reset position: {during_pause:.2} → {after_resume:.2}"
    );
    assert!(
        after_resume > during_pause,
        "resume didn't advance position: {during_pause:.2} → {after_resume:.2}"
    );

    let duration_0 = ctx
        .queue
        .duration_seconds()
        .expect("duration for first track");
    let seek_target = duration_0 * 0.4;
    ctx.queue.seek(seek_target).expect("seek");
    wait_for_position_near(&ctx.queue, seek_target, 1.0, Duration::from_secs(5))
        .await
        .expect("seek landed near target");

    wait_for_status(
        &mut rx,
        &ctx.queue,
        ids[1],
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("pre-crossfade: next track load [{}]: {e}", urls[1]));
    let xf_duration = ctx.queue.crossfade_duration();
    ctx.queue
        .advance_to_next(Transition::Crossfade, AdvanceReason::UserNext);
    let started = wait_for_queue_event(
        &mut rx,
        |ev| matches!(ev, QueueEvent::CrossfadeStarted { .. }),
        Duration::from_secs(10),
    )
    .await
    .expect("CrossfadeStarted event");
    if let QueueEvent::CrossfadeStarted { duration_seconds } = started {
        assert!(
            (duration_seconds - xf_duration).abs() < 0.01,
            "crossfade duration mismatch: event={duration_seconds:.2} vs config={xf_duration:.2}"
        );
    }
    wait_for_queue_event(
        &mut rx,
        |ev| matches!(ev, QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == ids[1]),
        Duration::from_millis((f64::from(xf_duration) * 1000.0) as u64 + 3_000),
    )
    .await
    .expect("CurrentTrackChanged to track 1 after crossfade");

    let mut per_track: Vec<(String, Result<(), String>)> = Vec::new();
    for i in 1..urls.len() {
        let url = urls[i];
        let result: Result<(), String> =
            async {
                wait_for_status(
                    &mut rx,
                    &ctx.queue,
                    ids[i],
                    TrackStatus::Loaded,
                    Duration::from_secs(30),
                )
                .await
                .map_err(|e| format!("load: {e}"))?;
                wait_for_position_at_least(&ctx.queue, 2.0, Duration::from_secs(15))
                    .await
                    .map_err(|e| format!("play: {e}"))?;

                if i + 1 < urls.len() {
                    let dur = ctx
                        .queue
                        .duration_seconds()
                        .ok_or_else(|| "duration unknown".to_string())?;
                    let near_end = (dur - f64::from(xf_duration) - 2.0).max(0.0);
                    ctx.queue.seek(near_end).map_err(|e| format!("seek: {e}"))?;
                    wait_for_queue_event(
                    &mut rx,
                    |ev| matches!(
                        ev,
                        QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == ids[i + 1]
                    ),
                    Duration::from_secs(20),
                )
                .await
                .ok_or_else(|| "timeout on auto-advance".to_string())?;
                }
                Ok(())
            }
            .await;
        per_track.push((url.to_string(), result));
    }

    let last_result: Result<(), String> = async {
        let dur = ctx
            .queue
            .duration_seconds()
            .ok_or_else(|| "duration unknown".to_string())?;
        ctx.queue
            .seek((dur - 3.0).max(0.0))
            .map_err(|e| format!("seek: {e}"))?;
        wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::QueueEnded),
            Duration::from_secs(15),
        )
        .await
        .ok_or_else(|| "timeout on QueueEnded".to_string())?;
        Ok(())
    }
    .await;

    let mut fails: Vec<String> = per_track
        .iter()
        .filter_map(|(u, r)| r.as_ref().err().map(|e| format!("  - {u}: {e}")))
        .collect();
    if let Err(e) = &last_result {
        fails.push(format!(
            "  - [last:{}] QueueEnded: {e}",
            urls[urls.len() - 1]
        ));
    }
    if !fails.is_empty() {
        panic!(
            "queue_playlist_behavior: {} track(s) failed:\n{}",
            fails.len(),
            fails.join("\n")
        );
    }
}

/// REGRESSION (RED): cold-start latency across every DRM track captured
/// from the on-device session. Mirrors a user tapping through tracks —
/// each URL is a distinct asset (fresh cache-miss), so every start pays
/// master + variant playlists + DRM keys + first segment. Measures
/// append→first-audio per track and asserts it stays under the
/// acceptable-wait ceiling (1s on a normal connection). Currently RED:
/// on-device startup is ~4.5s, dominated by serialized variant-playlist
/// fetches.
// flash(false): real zvuk prod CDN/keyserver e2e; wall-clock latency IS the assertion.
#[kithara::test(tokio)]
#[ignore = "real zvuk prod DRM network (needs baked .env tokens) — run with --include-ignored"]
#[cfg(any(target_os = "macos", target_os = "ios"))]
async fn prod_tracks_sequential_startup_latency() {
    kithara_integration_tests::apple_warmup::warm_if_apple(DecoderBackend::Apple);

    const STARTUP_CEILING: Duration = Duration::from_secs(1);
    const TRACKS: &[&str] = &[
        "https://cdn-hls-slicer.zvuk.com/drm/track/104988976_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/113688673_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/123566675_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/131714156_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/135488625_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/136562115_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/137046708_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/138131437_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/138535172_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/139716840_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/140509143_3/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/141628267_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/142405787_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/142770592_3/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/143183529_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/145817161_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/148731554_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/159916835_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/163529263_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/164581725_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/165511278_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/169997048_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/171681646_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/172301616_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/172640775_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/179000327_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/180339527_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/181696305_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/181911634_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/182394791_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/182812078_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/183208054_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/53215370_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/53807581_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/74462582_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/78947600_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/79515355_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/84414146_2/master.m3u8",
    ];

    let ctx = shared_test_ctx().await;
    let mut report: Vec<(&'static str, Result<(Duration, Duration), String>)> =
        Vec::with_capacity(TRACKS.len());

    for url in TRACKS {
        let mut rx = ctx.queue.subscribe();
        let source = build_track_source(url, ctx, DecoderBackend::Apple, AbrMode::Auto(None));
        let t0 = kithara::platform::time::Instant::now();
        let track_id = ctx.queue.append(source);

        let outcome: Result<(Duration, Duration), String> = async {
            wait_for_status(
                &mut rx,
                &ctx.queue,
                track_id,
                TrackStatus::Loaded,
                Duration::from_secs(30),
            )
            .await
            .map_err(|e| format!("load: {e}"))?;
            let load_latency = t0.elapsed();

            ctx.queue
                .select(track_id, Transition::None)
                .map_err(|e| format!("select: {e:?}"))?;
            wait_for_position_at_least(&ctx.queue, 0.1, Duration::from_secs(20))
                .await
                .map_err(|e| format!("first-audio: {e}"))?;
            let first_audio_latency = t0.elapsed();

            Ok((load_latency, first_audio_latency))
        }
        .await;

        let _ = ctx.queue.remove(track_id);
        report.push((url, outcome));
    }

    let ceiling_ms = STARTUP_CEILING.as_millis();
    eprintln!(
        "\n=== prod sequential startup latency (append→first-audio), ceiling {ceiling_ms}ms ==="
    );
    let mut violations: Vec<String> = Vec::new();
    let mut first_audio_ms: Vec<f64> = Vec::new();
    for (url, outcome) in &report {
        match outcome {
            Ok((load, first_audio)) => {
                let load_ms = load.as_secs_f64() * 1000.0;
                let fa_ms = first_audio.as_secs_f64() * 1000.0;
                first_audio_ms.push(fa_ms);
                eprintln!("  {fa_ms:>7.0}ms first-audio ({load_ms:>7.0}ms load)  {url}");
                if *first_audio > STARTUP_CEILING {
                    violations.push(format!(
                        "  - {url}: first-audio={fa_ms:.0}ms > {ceiling_ms}ms"
                    ));
                }
            }
            Err(e) => {
                eprintln!("  FAIL {e}  {url}");
                violations.push(format!("  - {url}: {e}"));
            }
        }
    }
    if !first_audio_ms.is_empty() {
        let mut sorted = first_audio_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];
        let worst = sorted[sorted.len() - 1];
        let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
        eprintln!(
            "  -- n={} median={median:.0}ms mean={mean:.0}ms worst={worst:.0}ms --",
            sorted.len()
        );
    }
    assert!(
        violations.is_empty(),
        "{} of {} tracks exceeded {ceiling_ms}ms startup ceiling:\n{}",
        violations.len(),
        report.len(),
        violations.join("\n")
    );
}
