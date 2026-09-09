#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    decode::DecoderBackend,
    events::{AbrMode, AdvanceReason, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    platform::{
        time::{Duration, sleep, timeout},
        tokio::sync::OnceCell,
    },
    queue::{QueueControl, TrackSource, Transition},
};
use kithara_app::{document::Config, pools::AppPools};
use kithara_integration_tests::{
    Xorshift64, kithara,
    offline::{AppQueueFixture, insecure_app_queue, offline_gain_window},
    waits::{wait_for_position_at_least, wait_for_position_near},
};

/// Same as [`build_source`] but overrides `store.cache_dir` with this
/// process's private temp dir so the real `kithara-app` cache stays
/// clean.
fn build_track_source(
    url: &str,
    ctx: &AppQueueFixture,
    backend: DecoderBackend,
    abr: AbrMode,
) -> TrackSource<AppPools> {
    super::source_helper::app_track_source(
        url,
        &ctx.config,
        super::source_helper::app_disk_asset_store(&ctx.config, ctx.cache.path()),
        backend,
        abr,
        None,
    )
}

mod test_statics {
    use super::*;
    pub(super) static TEST_CTX: OnceCell<AppQueueFixture> = OnceCell::const_new();
}

async fn shared_test_ctx() -> &'static AppQueueFixture {
    test_statics::TEST_CTX.get_or_init(insecure_app_queue).await
}

async fn wait_for_status(
    rx: &mut EventReceiver,
    queue: &QueueControl<AppPools>,
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

async fn sample_positions(
    queue: &QueueControl<AppPools>,
    count: usize,
    interval: Duration,
) -> Vec<f64> {
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
#[case::zvuk_drm_1_symphonia(
    "https://cdn-hls-slicer.zvuk.com/drm/track/5807750_3/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_drm_1_apple(
        "https://cdn-hls-slicer.zvuk.com/drm/track/5807750_3/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_drm_1_android(
        "https://cdn-hls-slicer.zvuk.com/drm/track/5807750_3/master.m3u8",
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
#[case::zvuk_drm_2_symphonia(
    "https://cdn-hls-slicer.zvuk.com/drm/track/160830411_2/master.m3u8",
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_drm_2_apple(
        "https://cdn-hls-slicer.zvuk.com/drm/track/160830411_2/master.m3u8",
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_drm_2_android(
        "https://cdn-hls-slicer.zvuk.com/drm/track/160830411_2/master.m3u8",
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
    let track_id = ctx
        .queue
        .run(move |q| q.append(source))
        .await
        .expect("append real playlist track");

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
        .run(move |q| q.select(track_id, Transition::None))
        .await
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
        ctx.queue.run(move |q| q.seek(target)).await.expect("seek");
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
    let pacing = ctx.queue.host().pacing().expect("paced offline queue");
    let gain_window = offline_gain_window(
        2.0,
        ctx.queue.host().spec().sample_rate,
        ctx.queue.host().max_block_frames(),
        pacing,
    );
    assert!(
        gain_window.contains(&gain),
        "position gain out of offline-realtime window [{url}]: got \
         {gain:.2}s over 2s wall clock (expected {gain_window:?}; start=\
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
    res.unwrap_or_else(|_| panic!("no matching queue event within {deadline:?}"))
}

/// Drive the shipped playlist (all its URLs, including DRM) end-
/// to-end: play first, pause/resume, seek, manual crossfade, auto-
/// advance through the rest, `QueueEnded` on the last. Per-track
/// failures are collected and reported in a structured final panic
/// so DRM regressions surface as a list instead of killing the whole
/// test at the first bad entry.
// flash(false): real-CDN e2e; sleeps are wall-clock pause/gain windows racing real sockets.
#[kithara::test(tokio)]
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
    let urls = Config::load(None, None)
        .expect("the shipped configuration loads")
        .tracks()
        .to_vec();
    assert!(urls.len() >= 3, "need ≥3 tracks for scenario");

    ctx.queue.set_crossfade_duration(2.0);

    let mut rx = ctx.queue.subscribe();
    let mut ids = Vec::with_capacity(urls.len());
    for url in &urls {
        let source = build_track_source(url, ctx, backend, AbrMode::Auto(None));
        let id = ctx
            .queue
            .run(move |q| q.append(source))
            .await
            .expect("append playlist track");
        ids.push(id);
    }

    ctx.queue
        .run({
            let arg0 = ids[0];
            move |q| q.select(arg0, Transition::None)
        })
        .await
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
    ctx.queue.run(move |q| q.pause()).await;
    time::sleep(Duration::from_secs(2)).await;
    let during_pause = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        (during_pause - before_pause).abs() < 0.5,
        "position drifted during pause: {before_pause:.2} → {during_pause:.2}"
    );
    ctx.queue.run(move |q| q.play()).await;
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
    ctx.queue
        .run(move |q| q.seek(seek_target))
        .await
        .expect("seek");
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
        .advance_to_next(Transition::Crossfade, AdvanceReason::UserNext)
        .expect("advance real-playlist crossfade");
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
        let url = &urls[i];
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
                    ctx.queue
                        .run(move |q| q.seek(near_end))
                        .await
                        .map_err(|e| format!("seek: {e}"))?;
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
        let track_id = ctx
            .queue
            .run(move |q| q.append(source))
            .await
            .expect("append Apple production track");

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
                .run(move |q| q.select(track_id, Transition::None))
                .await
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

/// The outgoing track's own timeline, seen at the seam.
struct Seam {
    left_at: f64,
    reason: Option<AdvanceReason>,
}

/// Follow `outgoing` until the queue leaves it, and report where its
/// clock stood at that moment. The seam is taken at the earliest signal
/// the queue gives - the crossfade starting, or the advance itself -
/// because that is when a listener first hears the outgoing track go.
async fn seam_out_of(
    queue: &QueueControl<AppPools>,
    rx: &mut EventReceiver,
    outgoing: TrackId,
    deadline: Duration,
) -> Seam {
    use kithara::platform::tokio::sync::broadcast::error::TryRecvError;

    let started = kithara::platform::time::Instant::now();
    let mut left_at = queue.position_seconds().unwrap_or(0.0);
    while started.elapsed() < deadline {
        let mut reason = None;
        let mut left = false;
        loop {
            match rx.try_recv() {
                Ok(envelope) => match envelope.event {
                    Event::Queue(QueueEvent::CurrentTrackAdvance { id, reason: why })
                        if id != Some(outgoing) =>
                    {
                        reason = Some(why);
                        left = true;
                    }
                    Event::Queue(QueueEvent::CrossfadeStarted { .. }) => left = true,
                    _ => {}
                },
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        if left || queue.current().is_none_or(|entry| entry.id != outgoing) {
            return Seam { left_at, reason };
        }
        left_at = queue.position_seconds().unwrap_or(left_at);
        sleep(Duration::from_millis(50)).await;
    }
    panic!("queue never left the outgoing track; its clock stood at {left_at:.2}s");
}

/// The seam the reported premature switch was seen on: an HLS stream
/// handing over to a whole MPEG body over HTTP. Nothing else crosses it -
/// every other case in this file loads one real source as a queue of one,
/// and the offline census alternates HLS with FLAC.
///
/// The measurement is where the outgoing track's clock stood when the
/// queue left it. A handover belongs inside the crossfade the queue
/// announced, so that reading must be within one crossfade of the
/// track's own length. A switch a listener hears as a fade in the middle
/// of a track lands far short of it, and the failure names the second it
/// happened on.
// flash(false): real-CDN e2e; the outgoing track is played through in wall clock.
#[kithara::test(tokio)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn hls_hands_over_to_mpeg_at_its_own_end(#[case] backend: DecoderBackend) {
    /// Sixty seconds of tones in ten segments, the shortest real HLS
    /// stream this playlist has.
    const HLS_URL: &str = "https://stream.silvercomet.top/tones/master.m3u8";
    /// A whole MPEG body on the same host, range-capable.
    const MPEG_URL: &str = "https://stream.silvercomet.top/track.mp3";
    const CROSSFADE_SECS: f32 = 2.0;
    /// Covers the 50 ms sampling grid and the per-segment gapless trim
    /// that shortens decoded audio below the `#EXTINF` sum.
    const SEAM_SLACK_SECS: f64 = 2.0;
    /// The stream's own media playlist: ten segments of six seconds. The
    /// seam reading is meaningless against a length the queue got wrong, so
    /// the length is a separate assertion - `PlayheadState::set_duration` is
    /// an unconditional store, and a decoder that speaks for one segment
    /// rather than the whole stream would lower it here.
    const HLS_LENGTH_SECS: f64 = 60.0;
    const LENGTH_SLACK_SECS: f64 = 1.0;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let ctx = shared_test_ctx().await;
    ctx.queue.set_crossfade_duration(CROSSFADE_SECS);

    let mut rx = ctx.queue.subscribe();
    let hls = ctx
        .queue
        .run({
            let source = build_track_source(HLS_URL, ctx, backend, AbrMode::Auto(None));
            move |q| q.append(source)
        })
        .await
        .expect("append the HLS leg");
    let mpeg = ctx
        .queue
        .run({
            let source = build_track_source(MPEG_URL, ctx, backend, AbrMode::Auto(None));
            move |q| q.append(source)
        })
        .await
        .expect("append the MPEG leg");

    wait_for_status(
        &mut rx,
        &ctx.queue,
        hls,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("HLS leg load [{HLS_URL}]: {e}"));
    ctx.queue
        .run(move |q| q.select(hls, Transition::None))
        .await
        .expect("select the HLS leg");
    wait_for_position_at_least(&ctx.queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("HLS leg never started [{HLS_URL}]: {e}"));

    let hls_duration = ctx
        .queue
        .duration_seconds()
        .expect("duration known after Loaded");
    assert!(
        (hls_duration - HLS_LENGTH_SECS).abs() <= LENGTH_SLACK_SECS,
        "the queue must report the length the stream's own playlist sums to: \
         reported={hls_duration:.2}s, playlist={HLS_LENGTH_SECS:.2}s"
    );
    let seam = seam_out_of(
        &ctx.queue,
        &mut rx,
        hls,
        Duration::from_secs_f64(hls_duration) + Duration::from_secs(30),
    )
    .await;

    assert_ne!(
        seam.reason,
        Some(AdvanceReason::TrackFailed),
        "the HLS leg was left because it failed, at {:.2}s of {hls_duration:.2}s",
        seam.left_at
    );
    let earliest = hls_duration - f64::from(CROSSFADE_SECS) - SEAM_SLACK_SECS;
    assert!(
        seam.left_at >= earliest,
        "the queue left the HLS leg at {:.2}s of {hls_duration:.2}s, {:.2}s before \
         the crossfade it announced could start (reason={:?})",
        seam.left_at,
        earliest - seam.left_at,
        seam.reason
    );

    let _ = ctx.queue.remove(hls);
    let _ = ctx.queue.remove(mpeg);
}
