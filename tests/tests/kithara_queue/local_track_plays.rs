#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara::{
    assets::StoreOptions,
    decode::DecoderBackend,
    events::{AbrMode, AudioEvent, Event, EventReceiver, QueueEvent, TrackId},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, sleep, timeout},
        tokio,
        tokio::sync::broadcast::error::{RecvError, TryRecvError},
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, Xorshift64,
    fixture_protocol::EncryptionRequest,
    kithara,
    offline::OfflineSession,
    temp_dir,
    waits::{wait_for_loader_done_event, wait_for_position_event, wait_for_position_near_event},
};
use url::Url;

#[derive(Clone, Copy, Debug)]
enum LocalSource {
    Mp3,
    HlsAac,
    HlsAacAes128,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").expect("hex write");
    }
    s
}

async fn build_fixture_url(kind: LocalSource, helper: &TestServerHelper) -> Url {
    match kind {
        LocalSource::Mp3 => helper.asset("track.mp3"),
        LocalSource::HlsAac => {
            let builder = HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(16)
                .segment_duration_secs(4.0)
                .packaged_audio_aac_lc(44_100, 2);
            helper
                .create_hls(builder)
                .await
                .expect("create local plain HLS fixture")
                .master_url()
        }
        LocalSource::HlsAacAes128 => {
            let key: &[u8] = b"0123456789abcdef";
            let iv: [u8; 16] = [0u8; 16];
            let builder = HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(16)
                .segment_duration_secs(4.0)
                .packaged_audio_aac_lc(44_100, 2)
                .encryption(EncryptionRequest {
                    key_hex: hex_encode(key),
                    iv_hex: Some(hex_encode(&iv)),
                });
            helper
                .create_hls(builder)
                .await
                .expect("create local encrypted HLS fixture")
                .master_url()
        }
    }
}

/// Drain `rx` non-blockingly and return the most recent sink-truth
/// `PlaybackProgress` position seen (seconds), or `None` if none is
/// buffered. Reads an event-sourced position WITHOUT consuming virtual
/// time — the caller brackets it around a body-level (virtualized)
/// `time::sleep`. Deliberately does NOT fall back to the tick cache: the
/// REAL-clock background tick loop refreshes that cache, so it goes stale
/// once an event-driven wait collapses real time. Callers decide what an
/// empty buffer means (a live-playback window waits for the next event; a
/// PAUSE window — where progress is silent by design — reads the frozen
/// `Queue::position_seconds()` explicitly).
fn drain_latest_position(rx: &mut EventReceiver) -> Option<f64> {
    let mut latest: Option<f64> = None;
    loop {
        match rx.try_recv() {
            Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                latest = Some(position_ms as f64 / 1000.0);
            }
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    latest
}

/// Block for the next sink-truth `PlaybackProgress` and return its
/// position (seconds). `recv()` parks on the virtual clock, so the offline
/// render worker advances and a fresh progress event is emitted; this
/// anchors a window endpoint to the SAME event clock as `drain_latest_position`
/// instead of the REAL-clock-gated tick cache. A buffered event already
/// past is fine — it is still event-sourced and on the render cadence.
async fn next_progress_position(rx: &mut EventReceiver, deadline: Duration) -> Result<f64, String> {
    timeout(deadline, async {
        loop {
            match rx.recv().await {
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                    return Ok(position_ms as f64 / 1000.0);
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            }
        }
    })
    .await
    .map_err(|_| format!("no PlaybackProgress within {deadline:?}"))?
}

/// Collect `count` playback positions sampled on consecutive sink-truth
/// `PlaybackProgress` events (real PCM-commit cadence under the virtual
/// clock), then assert non-decreasing. Replaces a wall-clock sample loop
/// whose REAL `sleep` would not park the engine. The fast-path seed keeps
/// the first sample aligned with the current head.
async fn sample_positions_via_progress(
    rx: &mut EventReceiver,
    queue: &Queue,
    count: usize,
    deadline: Duration,
) -> Result<Vec<f64>, String> {
    let mut out = Vec::with_capacity(count);
    out.push(queue.position_seconds().unwrap_or(0.0));
    timeout(deadline, async {
        while out.len() < count {
            match rx.recv().await {
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                    out.push(position_ms as f64 / 1000.0);
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    out.push(queue.position_seconds().unwrap_or(0.0));
                }
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| {
        format!(
            "only sampled {} of {count} positions in {deadline:?}",
            out.len()
        )
    })??;
    Ok(out)
}

fn assert_monotonic_nondecreasing(samples: &[f64], label: &str) {
    for w in samples.windows(2) {
        assert!(
            w[1] >= w[0] - 0.05,
            "position regressed [{label}]: {samples:?}"
        );
    }
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Downloader,
    StoreOptions,
    tokio::task::JoinHandle<()>,
) {
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let store = StoreOptions::new(temp_dir.path());
    (queue, downloader, store, tick_handle)
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::mp3_symphonia(LocalSource::Mp3, 42, DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_apple(LocalSource::Mp3, 42, DecoderBackend::Apple, AbrMode::Auto(None))
)]
#[cfg_attr(
    target_os = "android",
    case::mp3_android(LocalSource::Mp3, 42, DecoderBackend::Android, AbrMode::Auto(None))
)]
#[case::hls_aac_symphonia(
    LocalSource::HlsAac,
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_aac_apple(LocalSource::HlsAac, 42, DecoderBackend::Apple, AbrMode::Auto(None))
)]
#[cfg_attr(
    target_os = "android",
    case::hls_aac_android(LocalSource::HlsAac, 42, DecoderBackend::Android, AbrMode::Auto(None))
)]
#[case::hls_aes_symphonia(
    LocalSource::HlsAacAes128,
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_aes_apple(
        LocalSource::HlsAacAes128,
        42,
        DecoderBackend::Apple,
        AbrMode::Auto(None)
    )
)]
#[cfg_attr(
    target_os = "android",
    case::hls_aes_android(
        LocalSource::HlsAacAes128,
        42,
        DecoderBackend::Android,
        AbrMode::Auto(None)
    )
)]
async fn local_track_plays_end_to_end(
    #[case] kind: LocalSource,
    #[case] rng_seed: u64,
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let url = build_fixture_url(kind, &helper).await;
    let label = format!("{kind:?}/{backend:?}");

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    let cfg = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .downloader(downloader.clone())
        .store(store)
        .decoder_backend(backend)
        .initial_abr_mode(abr)
        .build();
    let source = TrackSource::Config(Box::new(cfg));

    // Subscribe before the actions that drive loading / playback so no
    // status or progress event can slip in before the first `recv`. The
    // queue bus is the player bus (`Queue::new` clones `player.bus()`),
    // so audio sink-truth events arrive here too.
    let mut rx = queue.subscribe();

    let track_id = queue.append(source);

    wait_for_loader_done_event(&mut rx, &queue, track_id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("load fail [{label}]: {e}"));

    queue.select(track_id, Transition::None).expect("select");
    wait_for_position_event(&mut rx, &queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("play fail [{label}]: {e}"));
    let progress = sample_positions_via_progress(&mut rx, &queue, 5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("sample fail [{label}]: {e}"));
    assert_monotonic_nondecreasing(&progress, &label);

    let duration = queue
        .duration_seconds()
        .expect("duration known after Loaded");
    let mut rng = Xorshift64::new(rng_seed);
    for i in 0..3 {
        let target = duration * rng.range_f64(0.05, 0.95);
        queue.seek(target).expect("seek");
        // `before` / `after` come from the sink-truth events (the wait
        // helpers' return values), NOT the REAL-clock tick cache, which
        // goes stale once these waits collapse real time.
        let before =
            wait_for_position_near_event(&mut rx, &queue, target, 1.0, Duration::from_secs(5))
                .await
                .unwrap_or_else(|e| panic!("seek #{i} to {target:.1}s fail [{label}]: {e}"));
        let after = wait_for_position_event(&mut rx, &queue, before + 0.5, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("seek #{i} hang [{label}]: {e}"));
        assert!(
            after - before >= 0.5,
            "seek #{i} hang [{label}]: {before:.2}→{after:.2}"
        );
    }

    // Offline-realtime gain window. Seek to a deterministic early position
    // first: the random seek loop above can leave the head close to EOF
    // (`target` reaches `0.95 * duration`), and after natural EOF the
    // offline player correctly STOPS emitting `PlaybackProgress` — position
    // can no longer advance, so a window started there would read ~0 gain
    // through no fault of the render cadence. Anchoring at 25% of duration
    // guarantees a full 2s of remaining audio for every fixture.
    let window_anchor = duration * 0.25;
    queue.seek(window_anchor).expect("seek to window anchor");
    wait_for_position_near_event(&mut rx, &queue, window_anchor, 1.0, Duration::from_secs(5))
        .await
        .unwrap_or_else(|e| panic!("window anchor seek [{label}]: {e}"));

    // Flush any progress buffered while the anchor seek settled so the
    // start anchor is a genuinely FRESH post-seek event, not a stale frame
    // from before the seek landed.
    let _ = drain_latest_position(&mut rx);

    // The `time::sleep` is in the BODY, so the macro virtualizes it; over 2
    // virtual seconds the offline render worker (one 512-frame block per
    // 10ms virtual park) advances ~2s of audio. BOTH endpoints are
    // event-sourced on the same `PlaybackProgress` cadence — `start_pos`
    // blocks for the next progress event (parking the virtual clock so the
    // render worker is live), `end_pos` drains the latest progress buffered
    // across the window — so the comparison never touches the REAL-clock-gated
    // tick cache, which goes stale once these waits collapse real time.
    let start_pos = next_progress_position(&mut rx, Duration::from_secs(10))
        .await
        .unwrap_or_else(|e| panic!("window start anchor [{label}]: {e}"));
    time::sleep(Duration::from_secs(2)).await;
    // Playback is live across the window, so progress events are buffered:
    // `drain_latest_position` returns the latest of them (the window end)
    // without touching the tick-cache fallback. The explicit
    // `next_progress_position` guard covers the rare empty-buffer race so
    // `end_pos` is always event-sourced, never the stale cache.
    let end_pos = match drain_latest_position(&mut rx) {
        Some(pos) => pos,
        None => next_progress_position(&mut rx, Duration::from_secs(10))
            .await
            .unwrap_or_else(|e| panic!("window end anchor [{label}]: {e}")),
    };
    let gain = end_pos - start_pos;
    assert!(
        (0.9..=2.5).contains(&gain),
        "position gain out of offline-realtime window [{label}]: got \
         {gain:.2}s over 2s wall clock (start={start_pos:.2} end={end_pos:.2})"
    );

    queue.remove(track_id).expect("remove");
    tick_handle.abort();
}

async fn wait_for_queue_event<F>(
    rx: &mut EventReceiver,
    mut pred: F,
    deadline: Duration,
) -> Option<QueueEvent>
where
    F: FnMut(&QueueEvent) -> bool,
{
    timeout(deadline, async {
        loop {
            match rx.recv().await {
                Ok(Event::Queue(ev)) => {
                    if pred(&ev) {
                        return Some(ev);
                    }
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return None,
            }
        }
    })
    .await
    .unwrap_or(None)
}

fn playlist_snapshot(queue: &Queue, ids: &[TrackId]) -> String {
    let current = queue.current().map(|entry| (entry.id, entry.status));
    let statuses: Vec<String> = ids
        .iter()
        .map(|id| {
            let status = queue.track(*id).map_or_else(
                || "missing".to_string(),
                |entry| format!("{:?}", entry.status),
            );
            format!("{}:{status}", id.as_u64())
        })
        .collect();
    format!(
        "current={current:?}, player_index={:?}, pos={:?}, dur={:?}, statuses=[{}]",
        queue.current_index(),
        queue.position_seconds(),
        queue.duration_seconds(),
        statuses.join(", ")
    )
}

/// Mirror of `real_playlist::queue_playlist_behavior` against local fixtures.
///
/// Five-track playlist (mp3 → hls aac → hls aes → hls aac → mp3) drives
/// the production pipeline through pause/resume, seek, manual crossfade,
/// auto-advance and `QueueEnded`. Per-track failures are collected so DRM
/// or loader regressions surface as a structured panic instead of the
/// first bad entry killing the whole test.
///
/// All waits are event-driven so the test is flash-correct: the wait
/// helpers `.await` sink-truth bus events (`TrackStatusChanged`,
/// `PlaybackProgress`, `SeekComplete`, `CurrentTrackChanged`,
/// `QueueEnded`), which park on the virtual clock AND resolve on real
/// state — a REAL-clock poll loop in a helper (the macro rewrites only
/// the test body) would never park the quiescence engine, so the engine
/// could not advance and playback would never progress. Load-bearing
/// positions are read from the events themselves, not the tick-cached
/// `Queue::position_seconds()`, which is refreshed by the REAL-clock
/// background tick loop and goes stale once an event-driven wait
/// collapses real time. `wait_for_loader_done` accepts `Loaded |
/// Consumed` (the loader flips straight to `Consumed` when a
/// `pending_select` was queued for the same track).
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(45)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn local_queue_playlist_behavior(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let kinds = [
        LocalSource::Mp3,
        LocalSource::HlsAac,
        LocalSource::HlsAacAes128,
        LocalSource::HlsAac,
        LocalSource::Mp3,
    ];
    let mut urls: Vec<Url> = Vec::with_capacity(kinds.len());
    for &k in &kinds {
        urls.push(build_fixture_url(k, &helper).await);
    }

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    queue.set_crossfade_duration(2.0);

    let mut rx = queue.subscribe();
    let ids: Vec<TrackId> = urls
        .iter()
        .map(|u| {
            let cfg = ResourceConfig::for_src(u.as_str())
                .expect("valid fixture URL")
                .downloader(downloader.clone())
                .store(store.clone())
                .decoder_backend(backend)
                .initial_abr_mode(AbrMode::Auto(None))
                .build();
            queue.append(TrackSource::Config(Box::new(cfg)))
        })
        .collect();

    queue
        .select(ids[0], Transition::None)
        .expect("select first");
    wait_for_loader_done_event(&mut rx, &queue, ids[0], Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("first track load [{}]: {e}", urls[0]));
    wait_for_position_event(&mut rx, &queue, 2.0, Duration::from_secs(5))
        .await
        .expect("first track position");

    queue.pause();
    // Let the pause take effect on the same (virtual) clock as playback.
    // Raw `PlaybackProgress` carries no track id, so in a crossfade/preload
    // playlist the queue-visible pause position must come from the queue
    // snapshot, not from whichever track emitted the last audio event.
    time::sleep(Duration::from_secs(1)).await;
    let before_pause = queue.position_seconds().unwrap_or(0.0);
    time::sleep(Duration::from_secs(2)).await;
    let during_pause = queue.position_seconds().unwrap_or(0.0);
    assert!(
        (during_pause - before_pause).abs() < 0.5,
        "position drifted during pause: {before_pause:.2} → {during_pause:.2}"
    );
    queue.play();
    let after_resume = wait_for_position_event(
        &mut rx,
        &queue,
        during_pause + 0.01,
        Duration::from_secs(10),
    )
    .await
    .unwrap_or_else(|e| panic!("resume didn't advance position from {during_pause:.2}: {e}"));
    assert!(
        after_resume >= during_pause - 0.1,
        "resume reset position: {during_pause:.2} → {after_resume:.2}"
    );
    assert!(
        after_resume > during_pause,
        "resume didn't advance position: {during_pause:.2} → {after_resume:.2}"
    );

    let duration_0 = queue.duration_seconds().expect("duration for first track");
    let seek_target = duration_0 * 0.4;
    queue.seek(seek_target).expect("seek");
    wait_for_position_near_event(&mut rx, &queue, seek_target, 1.0, Duration::from_secs(5))
        .await
        .expect("seek landed near target");

    wait_for_loader_done_event(&mut rx, &queue, ids[1], Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("pre-crossfade: next track load [{}]: {e}", urls[1]));
    let xf_duration = queue.crossfade_duration();
    queue.advance_to_next(Transition::Crossfade);
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
        Duration::from_millis((f64::from(xf_duration) * 1000.0) as u64 + 5_000),
    )
    .await
    .expect("CurrentTrackChanged to track 1 after crossfade");

    let mut per_track: Vec<(String, Result<(), String>)> = Vec::new();
    for i in 1..urls.len() {
        let url = urls[i].clone();
        let result: Result<(), String> =
            async {
                wait_for_loader_done_event(&mut rx, &queue, ids[i], Duration::from_secs(30))
                    .await
                    .map_err(|e| format!("load: {e}"))?;
                wait_for_position_event(&mut rx, &queue, 2.0, Duration::from_secs(5))
                    .await
                    .map_err(|e| format!("play: {e}"))?;

                if i + 1 < urls.len() {
                    let dur = queue
                        .duration_seconds()
                        .ok_or_else(|| "duration unknown".to_string())?;
                    let near_end = (dur - f64::from(xf_duration) - 2.0).max(0.0);
                    queue.seek(near_end).map_err(|e| format!("seek: {e}"))?;
                    wait_for_queue_event(
                    &mut rx,
                    |ev| matches!(
                        ev,
                        QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == ids[i + 1]
                    ),
                    Duration::from_secs(5),
                )
                .await
                .ok_or_else(|| {
                    format!(
                        "timeout on auto-advance; {}",
                        playlist_snapshot(&queue, &ids)
                    )
                })?;
                }
                Ok(())
            }
            .await;
        per_track.push((url.to_string(), result));
    }

    let last_result: Result<(), String> = async {
        let dur = queue
            .duration_seconds()
            .ok_or_else(|| "duration unknown".to_string())?;
        queue
            .seek((dur - 3.0).max(0.0))
            .map_err(|e| format!("seek: {e}"))?;
        wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::QueueEnded),
            Duration::from_secs(5),
        )
        .await
        .ok_or_else(|| format!("timeout on QueueEnded; {}", playlist_snapshot(&queue, &ids)))?;
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
            "local_queue_playlist_behavior: {} track(s) failed:\n{}",
            fails.len(),
            fails.join("\n")
        );
    }

    tick_handle.abort();
}
