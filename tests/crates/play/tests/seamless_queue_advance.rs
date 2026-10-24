#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    decode::{GaplessMode, SilenceTrimParams},
    events::TrackId,
    platform::{
        time::{self, Duration, Instant},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{PlayerEvent, ResourceConfig, ResourceSrc},
    queue::{QueueControl, Transition},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    event::TestEvent,
    fixture_protocol::{
        GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
    },
    hls_fixture::create_test_downloader,
    offline::{
        OfflinePlayer, OfflinePlayerOptions, TimedPlayerEvent, append_source_loaded,
        offline_queue_fixture_with_options,
    },
};
use kithara_render::bridge::{DeckSnapshot, SlotState};
use kithara_test_fixtures::signal::{deinterleave_left, goertzel_magnitude, max_silence_run};
use kithara_test_utils::{TestTempDir, temp_dir};
use url::Url;

use crate::{
    bufpool_ext::TestPools,
    gapless_common::{
        AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_SEGMENTS,
        AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
    },
};

const BLOCK_FRAMES: u32 = 512;
const POST_ROLL_BLOCKS: usize = 8;
/// Anything below this absolute amplitude (after AAC re-encode) is treated as
/// silence. Picked low enough to ignore quantisation noise and high enough
/// that genuine sine peaks are never classified as silence.
const SILENCE_THRESHOLD: f32 = 1.0e-3;
/// Tone carried by the first queued item.
const GAPLESS_LEADING_TONE_HZ: f64 = 1_000.0;
/// Tone carried by the second queued item, far enough from the first that a
/// single analysis window tells them apart.
const GAPLESS_TRAILING_TONE_HZ: f64 = 3_000.0;
/// Analysis window on each side of a candidate handover frame.
const TONE_SCAN_WINDOW: usize = BLOCK_FRAMES as usize;

/// Two queued items deliver the same amount of audio and meet without a gap.
///
/// Each item is measured by the audio it delivered, not by the render span it
/// occupied: a starved fetch makes the mixer zero-fill mid-item, stretching the
/// span without adding audio. Under stress the first item has been seen taking
/// eleven thousand extra frames while the second, already prefetched, kept its
/// normal span - host starvation under load, not a queue-advance defect. The
/// seam assertion below still pins the gapless property itself.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(1),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_play=debug,kithara_stream=debug")
)]
async fn seamless_queue_advance_gapless_when_crossfade_is_zero(
    #[future(awt)] gapless_sources: (TestServerHelper, [Url; 2]),
    temp_dir: TestTempDir,
) {
    let (_server, [first_url, second_url]) = gapless_sources;
    let expected_visible_frames = crate::gapless_common::generated_aac_elst_visible_frames();
    let gapless_params = SilenceTrimParams::builder().trim_trailing(true).build();
    let player_config = OfflinePlayerOptions::builder()
        .crossfade_duration(0.0)
        .gapless_mode(GaplessMode::SilenceTrim(gapless_params))
        .build();
    let (harness, queue) =
        offline_queue_fixture_with_options(player_config, GAPLESS_SAMPLE_RATE).await;
    let first = append_source_loaded(
        &harness,
        &queue,
        gapless_hls_config(&first_url, temp_dir.path()),
    )
    .await;
    let second = append_source_loaded(
        &harness,
        &queue,
        gapless_hls_config(&second_url, temp_dir.path()),
    )
    .await;

    let (rendered, events, _) = render_until_second_item_end(&harness, &queue, first, second).await;
    assert!(
        item_end_frame(&events, first).is_some(),
        "first item must reach natural EOF; events={events:?}"
    );
    assert!(
        item_end_frame(&events, second).is_some(),
        "second item must reach natural EOF; events={events:?}"
    );
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    // Measure both track lengths from the rendered audio, not from the frame
    // stamps of the end-of-item events. Both identities reached natural EOF,
    // but where their notifications landed is not a track length. A notification
    // is published by
    // whichever render block observed EOF, so its stamp is quantised to the
    // block grid, and this asset is an exact multiple of that grid: both
    // stamps normally sit one block late and the delta lands on the tolerance
    // itself. One block of delivery jitter then fails a test whose audio is
    // untouched.
    let audible_start = first_audible_frame(&left)
        .expect("rendered audio must contain the first track after the queue starts");
    let handover = tone_switch_frame(&left, GAPLESS_SAMPLE_RATE)
        .expect("rendered audio must switch from the first tone to the second");
    let audible_end = last_audible_frame(&left)
        .expect("rendered audio must contain the second track before the post-roll");

    let gaps = silence_runs(&left, MIN_GAP_FRAMES);
    let track1_len = audio_frames(&gaps, audible_start, handover);
    let track2_len = audio_frames(&gaps, handover, audible_end);
    let length_delta = track1_len.abs_diff(track2_len);
    assert!(
        length_delta <= BLOCK_FRAMES as usize,
        "track lengths should match within one render block; \
         track1={track1_len}, track2={track2_len}, delta={length_delta}, \
         tolerance={}, audible_start={audible_start}, handover={handover}, \
         audible_end={audible_end}, rendered={rendered_frames}, \
         expected_visible_frames={expected_visible_frames}, \
         silence_runs={gaps:?}; events={events:?}",
        BLOCK_FRAMES,
        rendered_frames = left.len(),
    );

    let search_start = handover.saturating_sub(BLOCK_FRAMES as usize * 2);
    let search_end = handover
        .saturating_add(BLOCK_FRAMES as usize)
        .min(left.len());
    let max_silence = max_silence_run(&left, search_start, search_end, SILENCE_THRESHOLD);
    let max_silence_ms_x10 = max_silence * 10_000 / sample_rate.max(1);
    assert!(
        max_silence < BLOCK_FRAMES as usize,
        "boundary silence run must stay below one render block ({block} frames), \
         got {max_silence} frames (~{ms_int}.{ms_frac:01} ms) in [{search_start}..{search_end}); \
         events={events:?}",
        block = BLOCK_FRAMES as usize,
        ms_int = max_silence_ms_x10 / 10,
        ms_frac = max_silence_ms_x10 % 10
    );
    harness.close().await;
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(1),
    tracing("kithara_play=info")
)]
async fn seamless_queue_advance_overlaps_tracks_when_crossfade_is_non_zero(
    #[future(awt)] crossfade_sources: (TestServerHelper, [Url; 2]),
    temp_dir: TestTempDir,
) {
    let (_server, [first_url, second_url]) = crossfade_sources;
    let gapless_params = SilenceTrimParams::builder().trim_trailing(true).build();
    let player_config = OfflinePlayerOptions::builder()
        .crossfade_duration(1.0)
        .gapless_mode(GaplessMode::SilenceTrim(gapless_params))
        .build();
    let (harness, queue) =
        offline_queue_fixture_with_options(player_config, GAPLESS_SAMPLE_RATE).await;
    let first = append_source_loaded(
        &harness,
        &queue,
        gapless_hls_config(&first_url, temp_dir.path()),
    )
    .await;
    let second = append_source_loaded(
        &harness,
        &queue,
        gapless_hls_config(&second_url, temp_dir.path()),
    )
    .await;

    let (rendered, events, snapshots) =
        render_until_second_item_end(&harness, &queue, first, second).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    let (overlap_start, overlap_end) =
        assert_crossfade_lifecycle(&snapshots, &events, first, second, &left);

    let leading_solo_start = overlap_start.saturating_sub(sample_rate / 2);
    let leading_solo_end = overlap_start;
    let trailing_solo_start = overlap_end;
    let trailing_solo_end = trailing_solo_start
        .saturating_add(sample_rate / 2)
        .min(left.len());

    let spectral_window = BLOCK_FRAMES as usize;
    let overlap_440 = max_windowed_goertzel_magnitude(
        &left,
        overlap_start,
        overlap_end,
        440.0,
        GAPLESS_SAMPLE_RATE,
        spectral_window,
    );
    let overlap_880 = max_windowed_goertzel_magnitude(
        &left,
        overlap_start,
        overlap_end,
        880.0,
        GAPLESS_SAMPLE_RATE,
        spectral_window,
    );
    let solo_440 = max_windowed_goertzel_magnitude(
        &left,
        leading_solo_start,
        leading_solo_end,
        440.0,
        GAPLESS_SAMPLE_RATE,
        spectral_window,
    );
    let solo_880 = max_windowed_goertzel_magnitude(
        &left,
        trailing_solo_start,
        trailing_solo_end,
        880.0,
        GAPLESS_SAMPLE_RATE,
        spectral_window,
    );

    assert!(
        overlap_440 >= solo_440 * 0.2,
        "leading frequency must remain audible in overlap window; \
         overlap_440={overlap_440:.3}, solo_440={solo_440:.3}, events={events:?}"
    );
    assert!(
        overlap_880 >= solo_880 * 0.15,
        "trailing frequency must be audible before the outgoing slot stops; \
         overlap_880={overlap_880:.3}, solo_880={solo_880:.3}, events={events:?}"
    );

    let max_silence = max_silence_run(&left, overlap_start, overlap_end, SILENCE_THRESHOLD);
    assert!(
        max_silence < BLOCK_FRAMES as usize,
        "no extended silence allowed inside the overlap; \
         max_silence={max_silence} frames, overlap=[{overlap_start}..{overlap_end}), \
        events={events:?}"
    );
    harness.close().await;
}

fn assert_crossfade_lifecycle(
    snapshots: &[(usize, Option<TrackId>, DeckSnapshot)],
    events: &[TimedPlayerEvent],
    first: TrackId,
    second: TrackId,
    left: &[f32],
) -> (usize, usize) {
    let (_, _, first_playing) = snapshots
        .iter()
        .find(|(_, current, snapshot)| {
            *current == Some(first)
                && snapshot
                    .slots
                    .iter()
                    .filter(|slot| slot.state == SlotState::Playing)
                    .count()
                    == 1
        })
        .expect("current first item must be the only Playing physical slot before handover");
    let first_slot = first_playing
        .slots
        .iter()
        .position(|slot| slot.state == SlotState::Playing)
        .expect("first item has a Playing slot");
    let item2_activated = events
        .iter()
        .find(|timed| {
            matches!(
                &timed.event,
                PlayerEvent::CurrentItemChanged { item: Some(id) } if *id == second
            )
        })
        .map(|timed| timed.frame_end)
        .expect("CurrentItemChanged should fire when item-2 takes over");

    let overlap_start = item2_activated;
    let (_, _, both_playing) = snapshots
        .iter()
        .find(|(frame, current, snapshot)| {
            *frame >= overlap_start
                && *current == Some(second)
                && snapshot.slots[first_slot].state == SlotState::Playing
                && snapshot
                    .slots
                    .iter()
                    .filter(|slot| slot.state == SlotState::Playing)
                    .count()
                    == 2
                && snapshot
                    .slots
                    .iter()
                    .filter(|slot| slot.state == SlotState::Playing)
                    .all(|slot| slot.gain > 0.0)
        })
        .expect("both queue items must occupy Playing physical slots during the handover");
    let second_slot = both_playing
        .slots
        .iter()
        .enumerate()
        .find(|(index, slot)| *index != first_slot && slot.state == SlotState::Playing)
        .map(|(index, _)| index)
        .expect("current second item has a distinct Playing slot");
    let (outgoing_end, _, stopped) = snapshots
        .iter()
        .find(|(frame, current, snapshot)| {
            let outgoing = &snapshot.slots[first_slot];
            *frame > overlap_start
                && *current == Some(second)
                && ((outgoing.state == SlotState::Stopped && outgoing.gain == 0.0)
                    || outgoing.state == SlotState::Ended)
        })
        .expect("outgoing slot must reach zero-gain Stopped or its actual natural Ended state");
    if stopped.slots[first_slot].state == SlotState::Ended {
        assert!(
            item_end_frame(events, first).is_some(),
            "an outgoing Ended slot must publish its own natural EOF; events={events:?}"
        );
    }
    assert!(
        snapshots.iter().any(|(frame, current, snapshot)| {
            *frame > *outgoing_end
                && *current == Some(second)
                && snapshot.slots[first_slot].state == SlotState::Empty
                && snapshot.slots[second_slot].state == SlotState::Playing
        }),
        "outgoing slot must be released while the second item keeps playing; events={events:?}"
    );
    let overlap_end = (*outgoing_end).min(left.len());
    assert!(
        overlap_end > overlap_start,
        "overlap window bounded by the observed outgoing stop must be non-empty; \
         endpoint is sampled at render-block precision; \
         overlap=[{overlap_start}..{overlap_end}), events={events:?}"
    );
    assert_second_item_complete(snapshots, events, second, second_slot, left, overlap_end);

    (overlap_start, overlap_end)
}

fn assert_second_item_complete(
    snapshots: &[(usize, Option<TrackId>, DeckSnapshot)],
    events: &[TimedPlayerEvent],
    second: TrackId,
    second_slot: usize,
    left: &[f32],
    overlap_end: usize,
) {
    let item2_end = item_end_frame(events, second)
        .expect("the second item's own natural EOF must complete the queue");
    let (_, _, ended) = snapshots
        .iter()
        .find(|(_, _, snapshot)| snapshot.slots[second_slot].state == SlotState::Ended)
        .expect("second item's physical slot must reach natural EOF");
    let ended = &ended.slots[second_slot];
    let source = ended
        .source
        .expect("natural EOF retains terminal source facts");
    assert!(
        source.end_of_track,
        "second item must consume its terminal packet"
    );
    assert_eq!(
        ended
            .mark
            .expect("natural EOF retains a consumed mark")
            .position
            .checked_sub(
                source
                    .media_origin
                    .expect("fixture source has a known media origin")
            ),
        Some(
            source
                .media_end
                .expect("fixture source has a known media endpoint")
        ),
        "second item must reach the complete source endpoint"
    );
    let audible_end =
        last_audible_frame(left).expect("second item must remain audible after overlap");
    assert!(
        audible_end > overlap_end && audible_end <= item2_end,
        "second item must remain audible beyond the outgoing stop and finish by its own EOF; \
         audible_end={audible_end}, overlap_end={overlap_end}, item2_end={item2_end}"
    );
    assert!(
        left[item2_end..]
            .iter()
            .all(|sample| sample.abs() <= SILENCE_THRESHOLD),
        "the unchanged post-roll must be silent after the second item's natural EOF"
    );
}

fn gapless_hls_config(master: &Url, cache_dir: &std::path::Path) -> ResourceConfig<TestPools> {
    ResourceConfig::<TestPools>::for_src(
        ResourceSrc::parse(master.as_str()).expect("valid HLS master URL"),
    )
    .downloader(create_test_downloader())
    .store(kithara_integration_tests::disk_asset_store(cache_dir))
    .build()
}

/// Renders through the second item's natural EOF and post-roll, recording
/// player events and the physical slot state at each render-block endpoint.
#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
async fn render_until_second_item_end(
    harness: &OfflinePlayer,
    queue: &QueueControl<TestPools>,
    first: TrackId,
    second: TrackId,
) -> (
    Vec<f32>,
    Vec<TimedPlayerEvent>,
    Vec<(usize, Option<TrackId>, DeckSnapshot)>,
) {
    let mut rx = queue.subscribe();
    harness
        .run(queue, move |q| q.select(first, Transition::None))
        .await
        .expect("select the first queue item");
    let deadline = Instant::now() + Duration::from_secs(10);
    // Pace each rendered block at its real audio duration so the decode worker
    // stays ahead and the ring never underruns. Under flash a fixed sub-block
    // sleep (e.g. 5 ms) advances virtual time faster than one block of audio is
    // consumed, so the render loop outruns the producer — most visibly at the
    // gapless handover, inflating the second track with underrun zero-fill.
    let block_budget =
        Duration::from_secs_f64(f64::from(BLOCK_FRAMES) / f64::from(GAPLESS_SAMPLE_RATE));
    let mut rendered = Vec::new();
    let mut rendered_frames = 0usize;
    let mut events = Vec::new();
    let mut snapshots = Vec::new();

    let mut post_roll = None;
    let mut previous_position = queue.position_seconds();
    let mut previous_states = Vec::new();

    // WHY: The decode worker producing these blocks is uncounted real work.
    let _real_io = kithara::platform::flash::real_io();
    loop {
        hang_tick!(format!(
            "position={previous_position:?}, frames={rendered_frames}, events={events:?}"
        ));
        let _ = harness.run(queue, QueueControl::tick).await;
        let block = harness.render(BLOCK_FRAMES as usize).await;
        let snapshot = harness.deck_snapshot();
        let states: Vec<_> = snapshot.slots.iter().map(|slot| slot.state).collect();
        if states != previous_states {
            tracing::info!(target: "kithara_play", rendered_frames, slots = ?snapshot.slots, "queue handover slot states changed");
            previous_states = states;
        }
        let position = queue.position_seconds();
        if position != previous_position {
            previous_position = position;
            hang_reset!();
        }
        rendered.extend_from_slice(&block);
        rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES as usize);
        snapshots.push((
            rendered_frames,
            queue.current().map(|track| track.id),
            snapshot,
        ));
        loop {
            hang_tick!();
            match rx.try_recv().map(|envelope| envelope.event) {
                Ok(TestEvent::Player(event)) => {
                    hang_reset!();
                    events.push(TimedPlayerEvent::new(rendered_frames, event));
                }
                Ok(_) | Err(TryRecvError::Lagged(_)) => {
                    hang_reset!();
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }

        if post_roll.is_none() && item_end_frame(&events, second).is_some() {
            post_roll = Some(POST_ROLL_BLOCKS);
        }
        if let Some(blocks) = post_roll.as_mut() {
            if *blocks == 0 {
                hang_reset!();
                return (rendered, events, snapshots);
            }
            *blocks -= 1;
        }

        assert!(
            Instant::now() <= deadline,
            "timed out waiting for queue to finish; events={events:?}"
        );
        time::sleep(block_budget).await;
    }
}

fn item_end_frame(events: &[TimedPlayerEvent], id: TrackId) -> Option<usize> {
    events
        .iter()
        .find(|timed| {
            matches!(&timed.event, PlayerEvent::ItemDidPlayToEnd { item } if item.id() == id)
        })
        .map(|timed| timed.frame_end)
}

/// Frames the tone detector steps by when locating the handover. Smaller than
/// the analysis window so the located frame is not quantised to the render
/// block grid the delivery race lives on.
const TONE_SCAN_STEP: usize = 64;

/// Frame where the output stops carrying the first item's tone and starts
/// carrying the second's.
///
/// Both queued items are pure sines, so the handover is the frame that best
/// separates them: the leading tone behind it, the trailing tone ahead of it.
/// Scoring both sides of the candidate keeps the estimate unbiased — a
/// one-sided detector reports the boundary about half an analysis window late,
/// which on its own would consume the whole per-track tolerance.
fn tone_switch_frame(left: &[f32], sample_rate: u32) -> Option<usize> {
    let window = TONE_SCAN_WINDOW;
    let mut best_score = f64::NEG_INFINITY;
    let mut best_frame = None;
    let mut frame = window;
    while frame + window <= left.len() {
        let behind = &left[frame - window..frame];
        let ahead = &left[frame..frame + window];
        let score = goertzel_magnitude(behind, GAPLESS_LEADING_TONE_HZ, sample_rate)
            - goertzel_magnitude(behind, GAPLESS_TRAILING_TONE_HZ, sample_rate)
            + goertzel_magnitude(ahead, GAPLESS_TRAILING_TONE_HZ, sample_rate)
            - goertzel_magnitude(ahead, GAPLESS_LEADING_TONE_HZ, sample_rate);
        if score > best_score {
            best_score = score;
            best_frame = Some(frame);
        }
        frame += TONE_SCAN_STEP;
    }
    best_frame
}

/// First frame above the silence threshold, i.e. where the first item became
/// audible after any pre-roll the harness rendered while the queue loaded.
fn first_audible_frame(left: &[f32]) -> Option<usize> {
    left.iter()
        .position(|sample| sample.abs() > SILENCE_THRESHOLD)
}

/// One past the last frame above the silence threshold, i.e. the end of the
/// second item before the post-roll blocks.
fn last_audible_frame(left: &[f32]) -> Option<usize> {
    left.iter()
        .rposition(|sample| sample.abs() > SILENCE_THRESHOLD)
        .map(|index| index + 1)
}

/// Shortest silence run counted as a gap rather than a zero crossing.
///
/// A sine dips below the silence threshold around every zero crossing, and how
/// many consecutive samples land there depends on the tone: the two items carry
/// different frequencies, so counting sub-threshold samples directly would
/// measure the tone, not the audio. Two milliseconds is far longer than any
/// zero crossing at these frequencies and far shorter than one render block, so
/// only mixer silence is ever discounted.
const MIN_GAP_FRAMES: usize = 64;

/// Frames of audio delivered in `[start..end)`: the span minus the silence the
/// mixer emitted inside it.
fn audio_frames(gaps: &[(usize, usize)], start: usize, end: usize) -> usize {
    let silence: usize = gaps
        .iter()
        .map(|&(gap_start, len)| {
            let overlap_start = gap_start.max(start);
            let overlap_end = (gap_start + len).min(end);
            overlap_end.saturating_sub(overlap_start)
        })
        .sum();
    end.saturating_sub(start).saturating_sub(silence)
}

/// Runs of near-silence at least `min_len` frames long, as `(start, len)`.
///
/// Reported when a length assertion fails so the artifact shows where the audio
/// went missing instead of only that the totals disagree.
fn silence_runs(samples: &[f32], min_len: usize) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut run_start = None;
    for (index, sample) in samples.iter().enumerate() {
        if sample.abs() <= SILENCE_THRESHOLD {
            run_start.get_or_insert(index);
            continue;
        }
        if let Some(start) = run_start.take()
            && index - start >= min_len
        {
            runs.push((start, index - start));
        }
    }
    if let Some(start) = run_start
        && samples.len() - start >= min_len
    {
        runs.push((start, samples.len() - start));
    }
    runs
}

/// Strongest `freq_hz` Goertzel magnitude over `window_frames`-long windows of
/// `[start..end)`.
fn max_windowed_goertzel_magnitude(
    samples: &[f32],
    start: usize,
    end: usize,
    freq_hz: f64,
    sample_rate: u32,
    window_frames: usize,
) -> f64 {
    let end = end.min(samples.len());
    if end <= start {
        return 0.0;
    }

    let window_frames = window_frames.max(1);
    let mut best = 0.0_f64;
    let mut offset = start;
    while offset < end {
        let window_end = offset.saturating_add(window_frames).min(end);
        let magnitude = goertzel_magnitude(&samples[offset..window_end], freq_hz, sample_rate);
        if magnitude > best {
            best = magnitude;
        }
        if window_end == end {
            break;
        }
        offset = offset.saturating_add(window_frames);
    }
    best
}

async fn source_url(server: &TestServerHelper, signal: PackagedSignal, start_frame: u64) -> Url {
    let source = PackagedAudioSource::Signal(signal);
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: kithara::stream::AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: NonZeroU32::new(
                        u32::try_from(start_frame).expect("start_frame fits u32"),
                    ),
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: NonZeroU32::new(AAC_GAPLESS_ENCODER_DELAY),
                    trailing_delay: NonZeroU32::new(AAC_GAPLESS_TRAILING_DELAY),
                    source,
                    gapless_encoding: GaplessEncoding::default(),
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create seamless queue HLS fixture");

    created.master_url()
}

#[kithara::fixture]
async fn gapless_sources() -> (TestServerHelper, [Url; 2]) {
    let server = TestServerHelper::new().await;
    let first = source_url(
        &server,
        PackagedSignal::Sine {
            freq_hz: GAPLESS_LEADING_TONE_HZ,
        },
        0,
    )
    .await;
    let frames = crate::gapless_common::generated_aac_elst_visible_frames();
    let second = source_url(
        &server,
        PackagedSignal::Sine {
            freq_hz: GAPLESS_TRAILING_TONE_HZ,
        },
        u64::try_from(frames).expect("visible frame count fits u64"),
    )
    .await;
    (server, [first, second])
}

#[kithara::fixture]
async fn crossfade_sources() -> (TestServerHelper, [Url; 2]) {
    let server = TestServerHelper::new().await;
    let first = source_url(&server, PackagedSignal::Sine { freq_hz: 440.0 }, 0).await;
    let second = source_url(&server, PackagedSignal::Sine { freq_hz: 880.0 }, 0).await;
    (server, [first, second])
}
