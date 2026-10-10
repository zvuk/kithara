#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use kithara::{
    abr::AbrMode,
    audio::AudioEvent,
    decode::DecoderBackend,
    download::{DownloaderEvent, RequestId},
    hls::HlsEvent,
    platform::{
        time,
        time::{Duration, WallInstant},
        tokio,
        tokio::sync::broadcast::error::{RecvError, TryRecvError},
    },
    play::{ResourceConfig, ResourceSrc},
    queue::{QueueControl, TrackSource, Transition},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, event::TestEvent, fixture_protocol::DelayRule, kithara,
    offline::DiskQueue, usdt_trace, waits::wait_for_loader_done,
};
use kithara_test_utils::temp_dir;
use url::Url;

mod consts {
    use super::Duration;

    /// Big enough that "near end" is past any plausible initial-loading
    /// prefetch window AND the target segment cannot already be in
    /// flight at seek time.
    pub(super) const SEGMENT_COUNT: usize = 50;
    pub(super) const SEGMENT_DURATION_S: f64 = 4.0;
    /// Server-side artificial delay per segment. Picked much larger
    /// than the test-server roundtrip so initial-loading fetches are
    /// guaranteed in flight when the seek fires.
    pub(super) const SEGMENT_DELAY_MS: u64 = 800;
    /// Tightens the Downloader to a small concurrency so the
    /// stale-fetch starvation is observable. Production default is 5.
    pub(super) const MAX_CONCURRENT: usize = 3;
    /// Loader settle deadline.
    pub(super) const LOAD_DEADLINE: Duration = Duration::from_secs(20);
    /// Give-up deadline for collecting post-seek events, so a seek that never
    /// reaches the reader fails on the discriminating panic below instead of on
    /// the harness timeout. It bounds diagnostics, not the contract — the
    /// contract is bounded by this test's own `timeout(60s)`.
    ///
    /// Deliberately NOT derived from `SEGMENT_DELAY_MS`. That delay is served by
    /// the fixture's own HTTP thread, which a full-suite run starves along with
    /// everything else, so "four delay windows" stops being four windows of
    /// anything: the healthy path lands in well under a second solo and needed
    /// more than 3.2 s under suite contention, which is the same sequence of
    /// states, just later. A flat deadline an order of magnitude above the
    /// healthy latency keeps the panic precise without pinning the test to a
    /// clock the fixture cannot hold.
    pub(super) const POST_SEEK_OBSERVATION: Duration = Duration::from_secs(30);
    /// Allow 1 segment of HLS readahead before the `ReaderSeek` landing.
    pub(super) const WARMUP_TOLERANCE: usize = 1;
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
        .segments_per_variant(consts::SEGMENT_COUNT)
        .segment_duration_secs(consts::SEGMENT_DURATION_S)
        .packaged_audio_aac_lc(44_100, 2)
        .include_sidx(false)
        .push_delay_rule(DelayRule {
            variant: None,
            segment_eq: None,
            segment_gte: None,
            delay_ms: consts::SEGMENT_DELAY_MS,
        });
    helper
        .create_hls(builder)
        .await
        .expect("create HLS fixture")
        .master_url()
}

#[derive(Debug, Default)]
struct PostSeekObservation {
    /// First `ReaderSeek` event after `seek_at`. Confirms the decoder
    /// actually called `Seek::seek` on the stream (not just that
    /// `SeekControl::begin` ran).
    reader_seek: Option<TestEvent>,
    /// First `SegmentReadStart` after `seek_at`. The discriminating
    /// signal: a healthy seek path emits this with `segment_index ≈
    /// target`; a broken one emits it with `segment_index ∈ [0..3]`
    /// because the reader is still chewing through the prefix.
    first_segment_read_start: Option<TestEvent>,
    /// `RequestId`s of `RequestEnqueued` after `seek_at` whose URL
    /// resolves to a prefix segment (`segment_index < target -
    /// WARMUP_TOLERANCE`). Hard cap.
    prefix_enqueued_after_seek: HashSet<RequestId>,
    /// Whether a new-epoch (`RequestId` Enqueued after `seek_at`) fetch was
    /// observed to `RequestStarted` within the observation window, plus its
    /// `wait_in_queue` (kept for the diagnostic message only — NOT asserted on,
    /// because `wait_in_queue` is timed on the platform clock, virtual under
    /// flash, while the real HTTP contention runs on real time, so a duration
    /// deadline on it is incommensurate and flaky). The event-driven contract
    /// asserts the *presence* of this start, not its timing.
    target_started_wait: Option<Duration>,
}

#[kithara::test(
    tokio,
    multi_thread,
    serial,
    timeout(Duration::from_secs(60)),
    tracing("kithara_hls=debug,kithara_queue=debug,kithara_stream=debug")
)]
#[cfg_attr(not(target_os = "android"), case::symphonia(DecoderBackend::Symphonia))]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::default()))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn hls_seek_near_end_skips_prefix(
    #[future(awt)] prepared_hls: (TestServerHelper, Url),
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let probe_recorder = usdt_trace::scope();

    let (_server, url) = prepared_hls;

    let temp = temp_dir();
    let DiskQueue {
        queue,
        downloader,
        store,
        ticker: mut tick_handle,
        ..
    } = DiskQueue::builder(temp.path())
        .max_concurrent_downloads(consts::MAX_CONCURRENT)
        .open()
        .await;

    let mut rx = queue.subscribe();

    let cfg =
        ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("ResourceSrc::parse"))
            .downloader(downloader.clone())
            .store(store)
            .initial_abr_mode(AbrMode::Auto(None))
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .build();

    let track_id = queue
        .run(move |q| q.append(TrackSource::Config(Box::new(cfg))))
        .await
        .expect("append stale-fetch seek track");
    queue
        .run(move |q| q.select(track_id, Transition::None))
        .await
        .expect("select");
    queue.run(QueueControl::play).await;

    wait_for_loader_done(&queue, track_id, consts::LOAD_DEADLINE)
        .await
        .expect("loader settled");

    let mut pre_seek_enqueued: HashSet<RequestId> = HashSet::new();

    let _ = time::timeout(consts::LOAD_DEADLINE, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(TestEvent::Downloader(DownloaderEvent::RequestEnqueued {
                    request_id, ..
                })) => {
                    pre_seek_enqueued.insert(request_id);
                }
                Ok(TestEvent::Audio(AudioEvent::PlaybackProgress { position_ms, .. }))
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
            Ok(TestEvent::Downloader(DownloaderEvent::RequestEnqueued { request_id, .. })) => {
                pre_seek_enqueued.insert(request_id);
            }
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }

    let duration = queue.duration_seconds().expect("duration");
    let target_seconds = (duration - 0.5).max(0.0);

    // Partition probe firings into pre- vs post-seek by their position in the
    // scope's recorded order, NOT by timestamps: under flash the scheduler's
    // poll thread and the (virtual-clock) test body read incomparable clocks.
    // The scope records firings causally, so every firing after `queue.seek`
    // sits at an index past `pre_seek`.
    let pre_seek = probe_recorder.events().len();

    let seek_at = WallInstant::now();
    queue
        .run(move |q| q.seek(target_seconds))
        .await
        .expect("seek");

    let (observation, _reset_evt) = tokio::join!(
        observe_post_seek(&mut rx, seek_at, &pre_seek_enqueued),
        time::timeout(
            consts::POST_SEEK_OBSERVATION,
            probe_recorder.wait_for(|events| {
                events[pre_seek..]
                    .iter()
                    .any(|e| e.target == "kithara_hls_probe" && e.probe == "rebuild")
            }),
        ),
    );

    tick_handle.stop().await;

    let all_probe_events = probe_recorder.events();
    let total_probes = all_probe_events.len();
    let probe_events = &all_probe_events[pre_seek..];
    assert!(
        total_probes > 0,
        "[{backend:?}, probe] zero probe events captured — `usdt-probes` \
         feature not enabled in test build, or probe sites missing"
    );

    let post_seek_resets: Vec<_> = probe_events
        .iter()
        .filter(|e| e.target == "kithara_hls_probe" && e.probe == "rebuild")
        .collect();
    assert!(
        !post_seek_resets.is_empty(),
        "[{backend:?}, probe] hls_probe::rebuild never fired after \
         queue.seek — scheduler did not re-plan the target (total probes = {total_probes})"
    );
    let Some(TestEvent::Hls(HlsEvent::ReaderSeek {
        to_offset,
        variant: reader_variant,
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
    let target_floor = target_segment.saturating_sub(consts::WARMUP_TOLERANCE);
    assert!(
        target_floor >= consts::MAX_CONCURRENT,
        "[{backend:?}] ReaderSeek landed inside the initial prefix window: \
         segment={target_segment}, floor={target_floor}, max_concurrent={}",
        consts::MAX_CONCURRENT,
    );

    let reset = post_seek_resets
        .first()
        .expect("post-seek fetch plan rebuild");
    let scheduler_variant = reset.field("variant").expect("fetch plan variant");
    let scheduler_segment = reset.field("from_seg").expect("fetch plan target");
    assert_eq!(
        reader_variant.map(|variant| u64::try_from(variant).expect("variant fits u64")),
        Some(scheduler_variant),
        "the reader and the replacement fetch plan must name the same variant"
    );
    assert_eq!(
        scheduler_segment,
        u64::try_from(target_floor).expect("target floor fits u64"),
        "the replacement fetch plan must start at the reader's seek target: {scheduler_segment} vs {target_segment}"
    );
    let reset_index = probe_events
        .iter()
        .position(|event| std::ptr::eq(event, *reset))
        .expect("recorded rebuild belongs to this seek's probe window");
    let post_reset_events = &probe_events[reset_index + 1..];

    let post_seek_prefix_emissions: Vec<_> = post_reset_events
        .iter()
        .filter(|e| e.target == "kithara_hls_probe" && e.probe == "emit_fetch_cmd")
        .filter(|e| e.field("variant") == Some(scheduler_variant))
        .filter(|e| {
            e.field("segment_index").is_some_and(|s| {
                let seg = usize::try_from(s).unwrap_or(usize::MAX);
                seg < target_floor
            })
        })
        .collect();
    assert!(
        post_seek_prefix_emissions.is_empty(),
        "[bug, {backend:?}, probe-level defense-in-depth] {} `fetch_cmd_emitted` \
         events for prefix segments in the replacement fetch plan after ReaderSeek \
         landed at segment {target_segment} — scheduler walked through prefix \
         despite cursor reset. Sample seg indices: {:?}",
        post_seek_prefix_emissions.len(),
        post_seek_prefix_emissions
            .iter()
            .take(5)
            .filter_map(|e| e.field("segment_index"))
            .collect::<Vec<_>>(),
    );

    let target_emissions: Vec<_> = post_reset_events
        .iter()
        .filter(|e| e.target == "kithara_hls_probe" && e.probe == "emit_fetch_cmd")
        .filter(|e| e.field("variant") == Some(scheduler_variant))
        .filter(|e| {
            e.field("segment_index").is_some_and(|s| {
                let seg = usize::try_from(s).unwrap_or(0);
                seg >= target_floor
            })
        })
        .collect();
    assert!(
        !target_emissions.is_empty(),
        "[{backend:?}, probe] no `fetch_cmd_emitted` for ReaderSeek target segment {target_segment} \
         (or near it within WARMUP_TOLERANCE) in the replacement fetch plan — \
         scheduler did not emit a FetchCmd for the seek target"
    );

    let Some(TestEvent::Hls(HlsEvent::SegmentReadStart {
        segment_index: first_seg,
        ..
    })) = observation.first_segment_read_start
    else {
        panic!(
            "[{backend:?}] no HlsEvent::SegmentReadStart after seek — reader \
             did not start consuming any segment within {:?}",
            consts::POST_SEEK_OBSERVATION,
        );
    };
    assert!(
        first_seg >= target_floor,
        "[bug, {backend:?}] reader went to prefix segment {first_seg} after \
         seek to segment {target_segment} — the target byte range never \
         got a free slot, so the reader is consuming prefix bytes that \
         were already in flight when the seek fired"
    );

    assert!(
        observation.prefix_enqueued_after_seek.len() <= consts::MAX_CONCURRENT,
        "[bug, {backend:?}] {} new prefix RequestEnqueued events after seek \
         (cap = MAX_CONCURRENT = {})",
        observation.prefix_enqueued_after_seek.len(),
        consts::MAX_CONCURRENT,
    );

    // TestEvent-driven progress contract: the new epoch must START a download
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
        consts::POST_SEEK_OBSERVATION,
    );
    queue.close().await;
    drop(probe_recorder);
}

async fn observe_post_seek(
    rx: &mut kithara::events::EventReceiver<TestEvent>,
    _seek_at: WallInstant,
    pre_seek_enqueued: &HashSet<RequestId>,
) -> PostSeekObservation {
    let mut obs = PostSeekObservation::default();
    let mut new_epoch_enqueued: HashSet<RequestId> = HashSet::new();
    let mut enqueue_url: HashMap<RequestId, String> = HashMap::new();
    let mut target_segment: Option<usize> = None;

    // Collect until the two discriminating facts are observed — ReaderSeek and
    // the first post-seek SegmentReadStart — then exit immediately, picking up
    // the diagnostic-only RequestStarted on the way if it arrives first.
    //
    // `POST_SEEK_OBSERVATION` is a give-up budget, not a measurement. It does
    // NOT collapse on the flash clock the way an earlier revision claimed:
    // `time::timeout` is virtual only while flash is on, and the `--flash=off`
    // lane runs it against the wall — which is exactly the lane that can
    // observe this test's contract at all, since flash collapses the fixture's
    // per-segment delay and with it the queue ordering the seek path is
    // supposed to get right. A genuinely broken seek that never populates a
    // field still terminates here with a partial `obs`; the discriminating
    // panics live in the caller and fire on the missing field, so this give-up
    // never silently passes an assertion.
    let _ = time::timeout(consts::POST_SEEK_OBSERVATION, async {
        loop {
            match rx.recv().await {
                Ok(env) => match &env.event {
                    TestEvent::Hls(HlsEvent::ReaderSeek { segment_index, .. }) => {
                        if obs.reader_seek.is_none() {
                            target_segment = *segment_index;
                            obs.reader_seek = Some(env.event.clone());
                        }
                    }
                    TestEvent::Hls(HlsEvent::SegmentReadStart { .. }) => {
                        if obs.reader_seek.is_some() && obs.first_segment_read_start.is_none() {
                            obs.first_segment_read_start = Some(env.event.clone());
                        }
                    }
                    TestEvent::Downloader(DownloaderEvent::RequestEnqueued {
                        request_id,
                        url,
                        ..
                    }) => {
                        if !pre_seek_enqueued.contains(request_id) {
                            new_epoch_enqueued.insert(*request_id);
                            enqueue_url.insert(*request_id, url.to_string());
                            if let (Some(target), Some((_v, seg_idx))) =
                                (target_segment, parse_segment_url(url.as_str()))
                                && seg_idx + consts::WARMUP_TOLERANCE < target
                            {
                                obs.prefix_enqueued_after_seek.insert(*request_id);
                            }
                        }
                    }
                    TestEvent::Downloader(DownloaderEvent::RequestStarted {
                        request_id,
                        wait_in_queue,
                    }) if obs.target_started_wait.is_none()
                        && new_epoch_enqueued.contains(request_id) =>
                    {
                        obs.target_started_wait = Some(*wait_in_queue);
                    }
                    _ => {}
                },
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }

            // Exit on the facts the assertions read. `target_started_wait` is
            // diagnostic only, so waiting for it made termination depend on
            // something no assertion requires — a healthy run that never
            // produced it sat here until the deadline.
            if obs.reader_seek.is_some() && obs.first_segment_read_start.is_some() {
                break;
            }
        }
    })
    .await;

    obs
}

#[kithara::fixture]
async fn prepared_hls() -> (TestServerHelper, Url) {
    let helper = TestServerHelper::new().await;
    let url = build_hls_with_delay(&helper).await;
    (helper, url)
}
