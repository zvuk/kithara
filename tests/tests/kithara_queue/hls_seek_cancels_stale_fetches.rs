//! Red-test: when the user seeks near the end of an HLS track BEFORE
//! the initial-loading fetches have completed, the player must actually
//! jump to the target segment and start reading there — not wait for
//! the prefix segments to drain through `MAX_CONCURRENT` slots.
//!
//! Production scenario captured in `kithara-app/app.log`: user opens
//! a track, waits a fraction of a second, drags the playhead to the end.
//! With the seek-bug present, `epoch_cancel.cancel()` is unreachable
//! on the hot path, so stale prefix fetches keep all Downloader slots,
//! the target segment never starts, and the reader keeps consuming
//! prefix segments while the user waits.
//!
//! Reader-side decoder hooks (`HlsEvent::ReaderSeek`,
//! `SegmentReadStart`, `SegmentReadComplete`) plus the existing
//! `DownloaderEvent::*` lifecycle let us assert the contract directly,
//! without timing budgets:
//!
//! - **A.** `ReaderSeek` fired and landed in the target segment region.
//! - **B.** First `SegmentReadStart` after seek is at or near the target —
//!   the reader did **not** rewind into the prefix.
//! - **C.** Hard cap on post-seek prefix `RequestEnqueued` events:
//!   only what was already in flight at seek time can complete; no
//!   new prefix fetches are scheduled.
//! - **D.** Target's `RequestStarted.wait_in_queue` is below the
//!   slot-pressure threshold.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Once},
    time::{Duration, Instant},
};

use kithara_assets::StoreOptions;
use kithara_events::{AbrMode, DownloaderEvent, Event, HlsEvent, RequestId, TrackId, TrackStatus};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, internal::init_offline_backend};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::DelayRule, kithara,
    temp_dir,
};
use tokio::time::sleep;
use url::Url;

use crate::common::decoder_backend::DecoderBackend;

static INIT_OFFLINE: Once = Once::new();

struct Consts;
impl Consts {
    /// Big enough that "near end" is past any plausible initial-loading
    /// prefetch window AND the target segment cannot already be in
    /// flight at seek time.
    const SEGMENT_COUNT: usize = 50;
    const SEGMENT_DURATION_S: f64 = 4.0;
    /// Server-side artificial delay per segment. Picked much larger
    /// than the test-server roundtrip so initial-loading fetches are
    /// guaranteed in flight when the seek fires.
    const SEGMENT_DELAY_MS: u64 = 800;
    /// Tightens the Downloader to a small concurrency so the
    /// stale-fetch starvation is observable. Production default is 5.
    const MAX_CONCURRENT: usize = 3;
    /// Brief warmup so the player is in steady playback when the seek
    /// fires.
    const WARMUP_MS: u64 = 100;
    /// Loader settle deadline.
    const LOAD_DEADLINE: Duration = Duration::from_secs(20);
    /// Window for collecting post-seek events. Four delay windows is
    /// generous: enough for the reader to actually start consuming
    /// the target segment if the seek path works.
    const POST_SEEK_OBSERVATION: Duration = Duration::from_millis(Self::SEGMENT_DELAY_MS * 4);
    /// Deadline for target's `RequestStarted.wait_in_queue`. With a
    /// working seek path the target starts immediately; without — it
    /// waits for at least one full delay window.
    const TARGET_START_DEADLINE: Duration = Duration::from_millis(Self::SEGMENT_DELAY_MS / 2);
    /// Allow 1 segment of slack between the nominal target and the
    /// observed value (warmup vs `seek_at` race).
    const WARMUP_TOLERANCE: usize = 1;
}

fn parse_segment_url(url: &str) -> Option<(usize, usize)> {
    // /stream/{spec}/seg/v{variant}_{segment}.m4s
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

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Arc<PlayerImpl>,
    Downloader,
    StoreOptions,
    tokio::task::JoinHandle<()>,
) {
    let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
    let queue = Arc::new(Queue::new(
        QueueConfig::default().with_player(Arc::clone(&player)),
    ));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let downloader =
        Downloader::new(DownloaderConfig::default().with_max_concurrent(Consts::MAX_CONCURRENT));
    let store = StoreOptions::new(temp_dir.path());
    (queue, player, downloader, store, tick_handle)
}

async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if let Some(entry) = queue.track(track_id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                TrackStatus::Failed(err) => return Err(format!("loader failed: {err}")),
                _ => {}
            }
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "loader timeout {deadline:?} (last={:?})",
                queue.track(track_id).map(|e| e.status)
            ));
        }
        sleep(Duration::from_millis(40)).await;
    }
}

#[derive(Debug, Default)]
struct PostSeekObservation {
    /// First `ReaderSeek` event after `seek_at`. Confirms the decoder
    /// actually called `Seek::seek` on the stream (not just that
    /// `Timeline::initiate_seek` ran).
    reader_seek: Option<Event>,
    /// First `SegmentReadStart` after `seek_at`. The discriminating
    /// signal: a healthy seek path emits this with `segment_index ≈
    /// target`; a broken one emits it with `segment_index ∈ [0..3]`
    /// because the reader is still chewing through the prefix.
    first_segment_read_start: Option<Event>,
    /// `RequestId`s of `RequestEnqueued` after `seek_at` whose URL
    /// resolves to a prefix segment (`segment_index < target -
    /// WARMUP_TOLERANCE`). Hard cap.
    prefix_enqueued_after_seek: HashSet<RequestId>,
    /// `wait_in_queue` for the first `RequestStarted` after `seek_at`
    /// whose `RequestId` was Enqueued strictly after `seek_at`. Slot-
    /// pressure metric.
    target_started_wait: Option<Duration>,
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[case::apple(DecoderBackend::Apple)]
async fn hls_seek_near_end_skips_prefix(#[case] backend: DecoderBackend) {
    if backend.skip_if_unavailable() {
        return;
    }
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    let url = build_hls_with_delay(&helper).await;

    let temp = temp_dir();
    let (queue, player, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    // Subscribe BEFORE creating the track so we capture every event
    // from the very first `RequestEnqueued`.
    let mut rx = player.bus().subscribe();

    let mut cfg = ResourceConfig::new(url.as_str()).expect("ResourceConfig::new");
    cfg = cfg.with_downloader(downloader.clone());
    cfg.store = store;
    cfg.initial_abr_mode = AbrMode::Auto(None);
    cfg.prefer_hardware = backend.prefer_hardware();

    let track_id = queue.append(TrackSource::Config(Box::new(cfg)));
    queue.select(track_id, Transition::None).expect("select");

    wait_for_loader_done(&queue, track_id, Consts::LOAD_DEADLINE)
        .await
        .expect("loader settled");

    sleep(Duration::from_millis(Consts::WARMUP_MS)).await;

    // Phase A: drain pre-seek events to clear backlog and snapshot
    // which `RequestId`s were enqueued before seek_at — the post-seek
    // observation needs to identify "new epoch" requests.
    let mut pre_seek_enqueued: HashSet<RequestId> = HashSet::new();
    while let Ok(ev) = rx.try_recv() {
        if let Event::Downloader(DownloaderEvent::RequestEnqueued { request_id, .. }) = ev {
            pre_seek_enqueued.insert(request_id);
        }
    }

    // Phase B: seek instant.
    let duration = queue.duration_seconds().expect("duration");
    let target_seconds = (duration - 0.5).max(0.0);

    // Nominal target segment from the test fixture (50 segments × 4s).
    // The actual landed segment is read out of the `ReaderSeek` event
    // below; this is just a sanity bound for assert A.
    let nominal_target_segment = Consts::SEGMENT_COUNT.saturating_sub(1);

    let seek_at = Instant::now();
    queue.seek(target_seconds).expect("seek");

    // Phase C: collect post-seek events.
    let observation = observe_post_seek(&mut rx, seek_at, &pre_seek_enqueued).await;

    tick_handle.abort();

    // ── ASSERTIONS ───────────────────────────────────────────────────

    // Ассерт A: ReaderSeek прилетел и landed near the target segment.
    //           Подтверждает что декодер реально дёрнул Seek::seek.
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

    // Ассерт B (главный сигнал бага): первый SegmentReadStart после
    //          seek_at имеет segment_index >= target. На сломанном коде
    //          reader идёт в prefix (segment_index = 0..3) и ассерт
    //          падает.
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

    // Ассерт C: hard cap на post-seek prefix-RequestEnqueued. Только то
    //           что уже в полёте может завершиться; никаких новых
    //           prefix-fetch'ей.
    assert!(
        observation.prefix_enqueued_after_seek.len() <= Consts::MAX_CONCURRENT,
        "[bug, {backend:?}] {} new prefix RequestEnqueued events after seek \
         (cap = MAX_CONCURRENT = {})",
        observation.prefix_enqueued_after_seek.len(),
        Consts::MAX_CONCURRENT,
    );

    // Ассерт D: target started promptly. Slot pressure indicator.
    let wait = observation.target_started_wait.unwrap_or_else(|| {
        panic!(
            "[{backend:?}] no RequestStarted observed for any post-seek \
             RequestEnqueued — new epoch never started a fetch within {:?}",
            Consts::POST_SEEK_OBSERVATION,
        )
    });
    assert!(
        wait <= Consts::TARGET_START_DEADLINE,
        "[{backend:?}] target's wait_in_queue = {wait:?} (deadline {:?}) — \
         slot starvation: stale fetches still hold {}/{} slots",
        Consts::TARGET_START_DEADLINE,
        observation.prefix_enqueued_after_seek.len(),
        Consts::MAX_CONCURRENT,
    );
}

async fn observe_post_seek(
    rx: &mut kithara_events::EventReceiver,
    seek_at: Instant,
    pre_seek_enqueued: &HashSet<RequestId>,
) -> PostSeekObservation {
    let mut obs = PostSeekObservation::default();
    let mut new_epoch_enqueued: HashSet<RequestId> = HashSet::new();
    let mut enqueue_url: HashMap<RequestId, String> = HashMap::new();
    let mut target_segment: Option<usize> = None;

    let deadline = seek_at + Consts::POST_SEEK_OBSERVATION;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => match &ev {
                Event::Hls(HlsEvent::ReaderSeek { segment_index, .. }) => {
                    if obs.reader_seek.is_none() {
                        target_segment = *segment_index;
                        obs.reader_seek = Some(ev.clone());
                    }
                }
                Event::Hls(HlsEvent::SegmentReadStart { .. }) => {
                    if obs.reader_seek.is_some() && obs.first_segment_read_start.is_none() {
                        obs.first_segment_read_start = Some(ev.clone());
                    }
                }
                Event::Downloader(DownloaderEvent::RequestEnqueued {
                    request_id, url, ..
                }) => {
                    if !pre_seek_enqueued.contains(request_id) {
                        new_epoch_enqueued.insert(*request_id);
                        enqueue_url.insert(*request_id, url.to_string());
                        // Hard cap (assert C): if this enqueued request
                        // is for a prefix segment relative to target,
                        // record it.
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
                    if obs.target_started_wait.is_none() && new_epoch_enqueued.contains(request_id)
                    {
                        obs.target_started_wait = Some(*wait_in_queue);
                    }
                }
                _ => {}
            },
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    obs
}
