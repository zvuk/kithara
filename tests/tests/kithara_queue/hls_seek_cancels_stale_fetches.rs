#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use kithara::{
    assets::AssetStore,
    decode::DecoderBackend,
    events::{AbrMode, AudioEvent, DownloaderEvent, Event, HlsEvent, RequestId},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration, Instant, sleep},
        tokio,
        tokio::sync::broadcast::error::{RecvError, TryRecvError},
        traits::FromWithParams,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::DelayRule, kithara,
    offline::OfflineSession, temp_dir, waits::wait_for_loader_done,
};
use kithara_test_utils::probe::capture as probe_capture;
use url::Url;

struct Consts;
impl Consts {
    /// Loader settle deadline.
    const LOAD_DEADLINE: Duration = Duration::from_secs(20);
    /// Tightens the Downloader to a small concurrency so the
    /// stale-fetch starvation is observable. Production default is 5.
    const MAX_CONCURRENT: usize = 3;
    /// Window for collecting post-seek events. Four delay windows is
    /// generous: enough for the reader to actually start consuming
    /// the target segment if the seek path works.
    const POST_SEEK_OBSERVATION: Duration = Duration::from_millis(Self::SEGMENT_DELAY_MS * 4);
    /// Big enough that "near end" is past any plausible initial-loading
    /// prefetch window AND the target segment cannot already be in
    /// flight at seek time.
    const SEGMENT_COUNT: usize = 50;
    /// Server-side artificial delay per segment. Picked much larger
    /// than the test-server roundtrip so initial-loading fetches are
    /// guaranteed in flight when the seek fires.
    const SEGMENT_DELAY_MS: u64 = 800;
    const SEGMENT_DURATION_S: f64 = 4.0;
    /// Allow 1 segment of slack between the nominal target and the
    /// observed value (warmup vs `seek_at` race).
    const WARMUP_TOLERANCE: usize = 1;
}

fn parse_segment_url(url: &str) -> Option<(usize, usize)> {
    let after = url.split("/seg/v").nth(1)?;
    let stem = after.split(".m4s").next()?;
    let mut parts = stem.split('_');
    let variant = parts.next()?.parse().ok()?;
    let segment = parts.next()?.parse().ok()?;
    Some((variant, segment))
}

async fn build_hls_with_delay(helper: &TestServerHelper) -> Url {
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(Consts::SEGMENT_COUNT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_S)
        .packaged_audio_aac_lc(44_100, 2)
        .include_sidx(false)
        .push_delay_rule(DelayRule {
            variant: None,
            segment_eq: None,
            segment_gte: None,
            delay_ms: Consts::SEGMENT_DELAY_MS,
        });
    helper
        .create_hls(builder)
        .await
        .expect("create HLS fixture")
        .master_url()
}

/// Queue tick driver, flash-coherent: spawned through the platform chokepoint
/// (participant + ambient propagation) with `#[kithara::flash(true)]` so the
/// 50ms cadence rides the VIRTUAL clock under flash. A raw `tokio::spawn` plus
/// a bare `sleep` runs invisible to the engine — the spawned task never parks
/// on the virtual clock, so the scheduler poll loop never cycles to observe the
/// seek-epoch bump and `kithara_hls_probe::seek_epoch_reset` never fires.
#[kithara::flash(true)]
async fn drive_queue_ticks(queue: Arc<Queue>) {
    loop {
        sleep(Duration::from_millis(50)).await;
        if queue.tick().is_err() {
            break;
        }
    }
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Arc<PlayerImpl>,
    Downloader,
    AssetStore,
    tokio::task::JoinHandle<()>,
) {
    let store = kithara_integration_tests::disk_asset_store(temp_dir.path());
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::build(
        Arc::clone(&player),
        QueueConfig::default().with_store(store.clone()),
    ));
    let tick_handle = tokio::task::spawn(drive_queue_ticks(Arc::clone(&queue)));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .max_concurrent(Consts::MAX_CONCURRENT)
            .build(),
    );
    (queue, player, downloader, store, tick_handle)
}

#[derive(Debug, Default)]
struct PostSeekObservation {
    /// `RequestId`s of `RequestEnqueued` after `seek_at` whose URL
    /// resolves to a prefix segment (`segment_index < target -
    /// WARMUP_TOLERANCE`). Hard cap.
    prefix_enqueued_after_seek: HashSet<RequestId>,
    /// First `SegmentReadStart` after `seek_at`. The discriminating
    /// signal: a healthy seek path emits this with `segment_index ≈
    /// target`; a broken one emits it with `segment_index ∈ [0..3]`
    /// because the reader is still chewing through the prefix.
    first_segment_read_start: Option<Event>,
    /// First `ReaderSeek` event after `seek_at`. Confirms the decoder
    /// actually called `Seek::seek` on the stream (not just that
    /// `SeekControl::begin` ran).
    reader_seek: Option<Event>,
    /// Whether a new-epoch (`RequestId` Enqueued after `seek_at`) fetch was
    /// observed to `RequestStarted` within the observation window, plus its
    /// `wait_in_queue` (kept for the diagnostic message only — NOT asserted on,
    /// because `wait_in_queue` is timed on the platform clock, virtual under
    /// flash, while the real HTTP contention runs on real time, so a duration
    /// deadline on it is incommensurate and flaky). The event-driven contract
    /// asserts the *presence* of this start, not its timing.
    target_started_wait: Option<Duration>,
}

#[kithara::test(tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn hls_seek_near_end_skips_prefix(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let probe_recorder = probe_capture::install();

    let helper = TestServerHelper::new().await;
    let url = build_hls_with_delay(&helper).await;

    let temp = temp_dir();
    let (queue, player, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    let mut rx = player.bus().subscribe();

    let cfg = ResourceConfig::for_src(url.as_str())
        .expect("ResourceConfig::for_src")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader.clone())
        .store(store)
        .initial_abr_mode(AbrMode::Auto(None))
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .build();

    let track_id = queue.append(TrackSource::Config(Box::new(cfg)));
    queue.select(track_id, Transition::None).expect("select");

    wait_for_loader_done(&queue, track_id, Consts::LOAD_DEADLINE)
        .await
        .expect("loader settled");

    let mut pre_seek_enqueued: HashSet<RequestId> = HashSet::new();

    // Warmup = wait until the player is in *steady processor playback*, i.e.
    // the offline render loop has actually committed PCM for this track and
    // emitted `AudioEvent::PlaybackProgress` with a non-zero position. This
    // is the discriminating gate: `HlsEvent::SegmentReadStart` only proves
    // the stream layer is reading (it can fire during the up-front blocking
    // build in `Audio::new`, before the processor has the track in a playing
    // state). The seek path runs through the processor —
    // `apply_seek` only forwards `track.seek` for tracks in
    // `FadingIn`/`Playing`, and only that path reaches `Audio::seek ->
    // SeekControl::begin`, which bumps the stream seek epoch the HLS
    // scheduler observes as `seek_epoch_reset`. Seeking before the track is
    // actually rendering means `apply_seek` finds no eligible track, drops
    // the seek silently, the epoch never bumps, and the scheduler never sees
    // a new epoch (even while it stays busy emitting other probes). Gating on
    // `PlaybackProgress` removes that race. We keep recording every
    // `RequestEnqueued` seen on the way so the pre-seek baseline stays
    // complete. The `time::timeout` is a safety deadline bounding a hang, not
    // a pacing wait; under flash the `rx.recv().await` parks on the virtual
    // clock so the render cadence (and scheduler) advance between events.
    let _ = time::timeout(Consts::LOAD_DEADLINE, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::Downloader(DownloaderEvent::RequestEnqueued { request_id, .. })) => {
                    pre_seek_enqueued.insert(request_id);
                }
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }))
                    if position_ms > 0 =>
                {
                    break;
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    })
    .await;

    // Drain any events still buffered after steady playback so the pre-seek
    // enqueued baseline is complete before the seek fires.
    loop {
        match rx.try_recv().map(|env| env.event) {
            Ok(Event::Downloader(DownloaderEvent::RequestEnqueued { request_id, .. })) => {
                pre_seek_enqueued.insert(request_id);
            }
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }

    let duration = queue.duration_seconds().expect("duration");
    let target_seconds = (duration - 0.5).max(0.0);

    let nominal_target_segment = Consts::SEGMENT_COUNT.saturating_sub(1);

    // Partition probe firings into pre- vs post-seek by the process-wide
    // monotonic `seq` counter, NOT by `Instant` timestamps. Under flash the
    // probe's `at` is stamped on the HLS scheduler's poll thread while
    // `seek_at` is read in the (virtual-clock) test body; those two clocks are
    // not comparable, so `e.at >= seek_at` would drop the legitimately-fired
    // `seek_epoch_reset`. `seq` is incremented causally at each probe firing,
    // so a firing after `queue.seek` always has `seq > pre_seek_seq`.
    let pre_seek_seq = probe_recorder
        .snapshot()
        .iter()
        .filter_map(kithara_test_utils::probe::capture::ProbeEvent::seq)
        .max()
        .unwrap_or(0);

    let seek_at = Instant::now();
    queue.seek(target_seconds).expect("seek");

    // Observe post-seek bus events AND wait for the scheduler to record the
    // epoch reset, concurrently. `wait_for_probe_async` parks on the virtual
    // clock between probe-snapshot polls, which lets the flash engine advance
    // virtual time so the (flash-coherent) tick driver cycles the HLS
    // scheduler — that poll cycle is what fires
    // `kithara_hls_probe::seek_epoch_reset`. Without this parking the
    // scheduler never observes the new epoch under flash. The budget is a
    // virtual hang ceiling, not a pacing wait; the probe-fired assertion
    let (observation, _reset_evt) = tokio::join!(
        observe_post_seek(&mut rx, seek_at, &pre_seek_enqueued),
        probe_recorder.wait_for_probe_async(
            |e| {
                e.seq().is_some_and(|s| s > pre_seek_seq)
                    && e.target == "kithara_hls_probe"
                    && e.probe_name() == Some("seek_epoch_reset")
            },
            Consts::POST_SEEK_OBSERVATION,
        ),
    );

    tick_handle.abort();

    let probe_events = probe_recorder.snapshot();
    let total_probes = probe_events.len();
    assert!(
        total_probes > 0,
        "[{backend:?}, probe] zero probe events captured — `usdt-probes` \
         feature not enabled in test build, or probe sites missing"
    );

    let post_seek_resets: Vec<_> = probe_events
        .iter()
        .filter(|e| e.seq().is_some_and(|s| s > pre_seek_seq))
        .filter(|e| e.target == "kithara_hls_probe" && e.probe_name() == Some("seek_epoch_reset"))
        .collect();
    assert!(
        !post_seek_resets.is_empty(),
        "[{backend:?}, probe] hls_probe::seek_epoch_reset never fired after \
         queue.seek — scheduler did not observe a new epoch (total probes = {total_probes})"
    );
    let new_epoch = post_seek_resets
        .iter()
        .filter_map(|e| e.u64("seek_epoch"))
        .max()
        .expect("seek_epoch field present on seek_epoch_reset probe");

    let post_seek_prefix_emissions: Vec<_> = probe_events
        .iter()
        .filter(|e| e.seq().is_some_and(|s| s > pre_seek_seq))
        .filter(|e| e.target == "kithara_hls_probe" && e.probe_name() == Some("emit_fetch_cmd"))
        .filter(|e| e.u64("seek_epoch") == Some(new_epoch))
        .filter(|e| {
            e.u64("segment_index").is_some_and(|s| {
                let seg = usize::try_from(s).unwrap_or(usize::MAX);
                seg + Consts::WARMUP_TOLERANCE < nominal_target_segment
            })
        })
        .collect();
    assert!(
        post_seek_prefix_emissions.is_empty(),
        "[bug, {backend:?}, probe-level defense-in-depth] {} `fetch_cmd_emitted` \
         events for prefix segments in the new epoch ({new_epoch}) after seek to \
         nominal segment {nominal_target_segment} — scheduler walked through prefix \
         despite cursor reset. Sample seg indices: {:?}",
        post_seek_prefix_emissions.len(),
        post_seek_prefix_emissions
            .iter()
            .take(5)
            .filter_map(|e| e.u64("segment_index"))
            .collect::<Vec<_>>(),
    );

    let target_emissions: Vec<_> = probe_events
        .iter()
        .filter(|e| e.seq().is_some_and(|s| s > pre_seek_seq))
        .filter(|e| e.target == "kithara_hls_probe" && e.probe_name() == Some("emit_fetch_cmd"))
        .filter(|e| e.u64("seek_epoch") == Some(new_epoch))
        .filter(|e| {
            e.u64("segment_index").is_some_and(|s| {
                let seg = usize::try_from(s).unwrap_or(0);
                seg + Consts::WARMUP_TOLERANCE >= nominal_target_segment
            })
        })
        .collect();
    assert!(
        !target_emissions.is_empty(),
        "[{backend:?}, probe] no `fetch_cmd_emitted` for target segment {nominal_target_segment} \
         (or near it within WARMUP_TOLERANCE) in the new epoch ({new_epoch}) — \
         scheduler did not emit a FetchCmd for the seek target"
    );

    let Some(Event::Hls(HlsEvent::ReaderSeek {
        to_offset,
        segment_index,
        ..
    })) = observation.reader_seek
    else {
        panic!(
            "[{backend:?}] no HlsEvent::ReaderSeek after queue.seek — \
             decoder seek didn't fire (or didn't reach the stream layer)"
        );
    };
    let target_segment = segment_index.unwrap_or_else(|| {
        panic!(
            "[{backend:?}] ReaderSeek to_offset={to_offset} landed outside \
             any committed segment — segment map not ready / seek too early"
        )
    });
    assert!(
        target_segment + Consts::WARMUP_TOLERANCE >= nominal_target_segment,
        "[{backend:?}] ReaderSeek landed at segment {target_segment}, \
         expected near-end (≥ {} based on nominal target {nominal_target_segment})",
        nominal_target_segment.saturating_sub(Consts::WARMUP_TOLERANCE),
    );

    let Some(Event::Hls(HlsEvent::SegmentReadStart {
        segment_index: first_seg,
        ..
    })) = observation.first_segment_read_start
    else {
        panic!(
            "[{backend:?}] no HlsEvent::SegmentReadStart after seek — reader \
             did not start consuming any segment within {:?}",
            Consts::POST_SEEK_OBSERVATION,
        );
    };
    assert!(
        first_seg + Consts::WARMUP_TOLERANCE >= target_segment,
        "[bug, {backend:?}] reader went to prefix segment {first_seg} after \
         seek to segment {target_segment} — the target byte range never \
         got a free slot, so the reader is consuming prefix bytes that \
         were already in flight when the seek fired"
    );

    assert!(
        observation.prefix_enqueued_after_seek.len() <= Consts::MAX_CONCURRENT,
        "[bug, {backend:?}] {} new prefix RequestEnqueued events after seek \
         (cap = MAX_CONCURRENT = {})",
        observation.prefix_enqueued_after_seek.len(),
        Consts::MAX_CONCURRENT,
    );

    // Event-driven progress contract: the new epoch must START a download
    // (`RequestStarted`) within the bounded observation window. A seek that
    // dropped silently, or a target left permanently starved behind stale
    // fetches that never free a slot, would never start one. Asserting the
    // START *event* (presence) rather than a `wait_in_queue` duration is
    // clock-independent — `wait_in_queue` is timed on the platform clock
    // (virtual under flash) while the real HTTP contention runs on real time,
    // so a duration deadline is incommensurate and flaky. The "don't re-fetch
    // the prefix on a near-end seek" half of the contract is enforced by the
    // probe-level prefix-walk + `prefix_enqueued_after_seek` checks above; this
    // assertion only proves the target itself made forward progress.
    assert!(
        observation.target_started_wait.is_some(),
        "[{backend:?}] no RequestStarted observed for any post-seek \
         RequestEnqueued within {:?} — the new epoch never started a fetch \
         (seek dropped silently, or the target starved behind stale fetches)",
        Consts::POST_SEEK_OBSERVATION,
    );
}

async fn observe_post_seek(
    rx: &mut kithara::events::EventReceiver,
    _seek_at: Instant,
    pre_seek_enqueued: &HashSet<RequestId>,
) -> PostSeekObservation {
    let mut obs = PostSeekObservation::default();
    let mut new_epoch_enqueued: HashSet<RequestId> = HashSet::new();
    let mut enqueue_url: HashMap<RequestId, String> = HashMap::new();
    let mut target_segment: Option<usize> = None;

    // Collect until the three discriminating facts are observed — ReaderSeek,
    // the first post-seek SegmentReadStart, and the first post-seek
    // RequestStarted — then exit immediately. `POST_SEEK_OBSERVATION` is a
    // bounded *virtual* give-up budget (via `time::timeout`, which collapses on
    // the flash clock), NOT a real-wall window: `rx.recv().await` parks on the
    // virtual clock between events so the engine advances and delivers them. A
    // genuinely broken seek that never populates a field still terminates here
    // with a partial `obs`; the discriminating panics live in the caller and
    // fire on the missing field, so this give-up never silently passes an
    // assertion. Previously this loop spun on `Instant::now() < deadline`
    // (real wall time), incommensurable with the virtual event delivery.
    let _ = time::timeout(Consts::POST_SEEK_OBSERVATION, async {
        loop {
            match rx.recv().await {
                Ok(env) => match &env.event {
                    Event::Hls(HlsEvent::ReaderSeek { segment_index, .. }) => {
                        if obs.reader_seek.is_none() {
                            target_segment = *segment_index;
                            obs.reader_seek = Some(env.event.clone());
                        }
                    }
                    Event::Hls(HlsEvent::SegmentReadStart { .. }) => {
                        if obs.reader_seek.is_some() && obs.first_segment_read_start.is_none() {
                            obs.first_segment_read_start = Some(env.event.clone());
                        }
                    }
                    Event::Downloader(DownloaderEvent::RequestEnqueued {
                        request_id, url, ..
                    }) => {
                        if !pre_seek_enqueued.contains(request_id) {
                            new_epoch_enqueued.insert(*request_id);
                            enqueue_url.insert(*request_id, url.to_string());
                            if let (Some(target), Some((_v, seg_idx))) =
                                (target_segment, parse_segment_url(url.as_str()))
                                && seg_idx + Consts::WARMUP_TOLERANCE < target
                            {
                                obs.prefix_enqueued_after_seek.insert(*request_id);
                            }
                        }
                    }
                    Event::Downloader(DownloaderEvent::RequestStarted {
                        request_id,
                        wait_in_queue,
                    }) => {
                        if obs.target_started_wait.is_none()
                            && new_epoch_enqueued.contains(request_id)
                        {
                            obs.target_started_wait = Some(*wait_in_queue);
                        }
                    }
                    _ => {}
                },
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }

            if obs.reader_seek.is_some()
                && obs.first_segment_read_start.is_some()
                && obs.target_started_wait.is_some()
            {
                break;
            }
        }
    })
    .await;

    obs
}
