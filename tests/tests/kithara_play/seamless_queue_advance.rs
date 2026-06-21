#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara_assets::StoreOptions;
use kithara_decode::{GaplessMode, SilenceTrimParams};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{PackagedAudioRequest, PackagedAudioSource, PackagedSignal},
    temp_dir,
};
use kithara_platform::time::{self, Duration, Instant};
use kithara_play::{PlayerConfig, PlayerEvent, Resource, ResourceConfig};

use super::offline_player_harness::OfflinePlayerHarness;
use crate::gapless_common::{
    AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_SEGMENTS,
    AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
};

const BLOCK_FRAMES: u32 = 512;
const POST_ROLL_BLOCKS: usize = 8;
/// Anything below this absolute amplitude (after AAC re-encode) is treated as
/// silence. Picked low enough to ignore quantisation noise and high enough
/// that genuine sine peaks are never classified as silence.
const SILENCE_THRESHOLD: f32 = 1.0e-3;

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn seamless_queue_advance_gapless_when_crossfade_is_zero(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let expected_visible_frames = crate::gapless_common::generated_aac_elst_visible_frames();
    let gapless_params = SilenceTrimParams {
        trim_trailing: true,
        ..SilenceTrimParams::default()
    };
    let player_config = PlayerConfig::builder()
        .crossfade_duration(0.0)
        .gapless_mode(GaplessMode::SilenceTrim(gapless_params))
        .build();
    let harness = OfflinePlayerHarness::with_sample_rate(player_config, GAPLESS_SAMPLE_RATE);
    let first = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        PackagedSignal::Sine { freq_hz: 1_000.0 },
        0,
    )
    .await;
    let second = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        PackagedSignal::Sine { freq_hz: 3_000.0 },
        u64::try_from(expected_visible_frames).expect("visible frame count fits u64"),
    )
    .await;

    load_queue(&harness, [first, second]);

    let (rendered, events) = render_until_second_item_end(&harness).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    let item1_end_event = nth_item_end_frame(&events, 0)
        .expect("first item must emit ItemDidPlayToEnd before the queue completes");
    let item2_end_event = nth_item_end_frame(&events, 1)
        .expect("second item must emit ItemDidPlayToEnd before the queue completes");

    let track1_len = item1_end_event;
    let track2_len = item2_end_event - item1_end_event;
    let length_delta = track1_len.abs_diff(track2_len);
    assert!(
        length_delta <= BLOCK_FRAMES as usize,
        "track lengths should match within one render block; \
         track1={track1_len}, track2={track2_len}, delta={length_delta}, \
         tolerance={}; events={events:?}",
        BLOCK_FRAMES,
    );
    let _ = expected_visible_frames;

    let search_start = item1_end_event.saturating_sub(BLOCK_FRAMES as usize * 2);
    let search_end = item1_end_event
        .saturating_add(BLOCK_FRAMES as usize)
        .min(left.len());
    let max_silence = max_silence_run(&left, search_start, search_end);
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
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn seamless_queue_advance_overlaps_tracks_when_crossfade_is_non_zero(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let gapless_params = SilenceTrimParams {
        trim_trailing: true,
        ..SilenceTrimParams::default()
    };
    let player_config = PlayerConfig::builder()
        .crossfade_duration(1.0)
        .gapless_mode(GaplessMode::SilenceTrim(gapless_params))
        .build();
    let harness = OfflinePlayerHarness::with_sample_rate(player_config, GAPLESS_SAMPLE_RATE);
    let first = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        PackagedSignal::Sine { freq_hz: 440.0 },
        0,
    )
    .await;
    let second = create_gapless_hls_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        PackagedSignal::Sine { freq_hz: 880.0 },
        0,
    )
    .await;

    load_queue(&harness, [first, second]);

    let (rendered, events) = render_until_second_item_end(&harness).await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    let item1_end = nth_item_end_frame(&events, 0)
        .expect("first item must emit ItemDidPlayToEnd before the queue completes");
    let item2_activated = events
        .iter()
        .filter(|timed| matches!(&timed.event, PlayerEvent::CurrentItemChanged))
        .nth(1)
        .map(|timed| timed.frame_end)
        .expect("CurrentItemChanged should fire when item-2 takes over");

    assert!(
        item2_activated < item1_end,
        "item-2 must take over before item-1 reaches EOF for a real overlap; \
         item2_activated={item2_activated}, item1_end={item1_end}, events={events:?}"
    );

    let overlap_start = item2_activated;
    let overlap_end = item1_end.min(left.len());
    assert!(
        overlap_end > overlap_start,
        "overlap window must be non-empty; \
         overlap=[{overlap_start}..{overlap_end}), events={events:?}"
    );

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
        sample_rate,
        spectral_window,
    );
    let overlap_880 = max_windowed_goertzel_magnitude(
        &left,
        overlap_start,
        overlap_end,
        880.0,
        sample_rate,
        spectral_window,
    );
    let solo_440 = max_windowed_goertzel_magnitude(
        &left,
        leading_solo_start,
        leading_solo_end,
        440.0,
        sample_rate,
        spectral_window,
    );
    let solo_880 = max_windowed_goertzel_magnitude(
        &left,
        trailing_solo_start,
        trailing_solo_end,
        880.0,
        sample_rate,
        spectral_window,
    );

    assert!(
        overlap_440 >= solo_440 * 0.2,
        "leading frequency must remain audible in overlap window; \
         overlap_440={overlap_440:.3}, solo_440={solo_440:.3}, events={events:?}"
    );
    assert!(
        overlap_880 >= solo_880 * 0.15,
        "trailing frequency must be audible before the first item reaches EOF; \
         overlap_880={overlap_880:.3}, solo_880={solo_880:.3}, events={events:?}"
    );

    let max_silence = max_silence_run(&left, overlap_start, overlap_end);
    assert!(
        max_silence < BLOCK_FRAMES as usize,
        "no extended silence allowed inside the overlap; \
         max_silence={max_silence} frames, overlap=[{overlap_start}..{overlap_end}), \
         events={events:?}"
    );
}

async fn create_gapless_hls_resource(
    player: &kithara_play::PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &std::path::Path,
    signal: PackagedSignal,
    start_frame: u64,
) -> Resource {
    let source = PackagedAudioSource::Signal(signal);
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: kithara_stream::AudioCodec::AacLc,
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
                    gapless_encoding: Default::default(),
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create seamless queue HLS fixture");

    let store = StoreOptions::new(cache_dir);
    let mut config = ResourceConfig::for_src(created.master_url().as_str())
        .expect("valid HLS master URL")
        .store(store)
        .build();
    config = player.prepare_config(config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS resource for seamless queue fixture");
    let _ = resource.preload().await;
    resource
}

fn load_queue<const N: usize>(harness: &OfflinePlayerHarness, items: [Resource; N]) {
    harness.player().reserve_slots(items.len());
    for (index, resource) in items.into_iter().enumerate() {
        harness.player().replace_item(index, resource);
    }
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");
}

async fn render_until_second_item_end(
    harness: &OfflinePlayerHarness,
) -> (Vec<f32>, Vec<TimedPlayerEvent>) {
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

    loop {
        let block = harness.render(BLOCK_FRAMES as usize);
        rendered.extend_from_slice(&block);
        rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES as usize);
        events.extend(
            harness
                .tick_and_drain()
                .into_iter()
                .map(|event| TimedPlayerEvent {
                    frame_end: rendered_frames,
                    event,
                }),
        );

        if count_item_end(&events) >= 2 {
            for _ in 0..POST_ROLL_BLOCKS {
                let block = harness.render(BLOCK_FRAMES as usize);
                rendered.extend_from_slice(&block);
                rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES as usize);
                events.extend(
                    harness
                        .tick_and_drain()
                        .into_iter()
                        .map(|event| TimedPlayerEvent {
                            frame_end: rendered_frames,
                            event,
                        }),
                );
            }
            return (rendered, events);
        }

        assert!(
            Instant::now() <= deadline,
            "timed out waiting for queue to finish; events={events:?}"
        );
        time::sleep(block_budget).await;
    }
}

fn deinterleave_left(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame[0])
        .collect::<Vec<_>>()
}

#[derive(Clone, Debug)]
struct TimedPlayerEvent {
    frame_end: usize,
    event: PlayerEvent,
}

fn count_item_end(events: &[TimedPlayerEvent]) -> usize {
    events
        .iter()
        .filter(|timed| matches!(&timed.event, PlayerEvent::ItemDidPlayToEnd { .. }))
        .count()
}

fn nth_item_end_frame(events: &[TimedPlayerEvent], n: usize) -> Option<usize> {
    events
        .iter()
        .filter(|timed| matches!(&timed.event, PlayerEvent::ItemDidPlayToEnd { .. }))
        .nth(n)
        .map(|timed| timed.frame_end)
}

/// Longest run of near-zero samples in `[start..end)`. Used to detect audible
/// gaps without depending on phase alignment.
fn max_silence_run(samples: &[f32], start: usize, end: usize) -> usize {
    let end = end.min(samples.len());
    if end <= start {
        return 0;
    }
    let mut max_run = 0usize;
    let mut current = 0usize;
    for sample in &samples[start..end] {
        if sample.abs() < SILENCE_THRESHOLD {
            current += 1;
            if current > max_run {
                max_run = current;
            }
        } else {
            current = 0;
        }
    }
    max_run
}

fn goertzel_magnitude(samples: &[f32], freq_hz: f64, sample_rate: usize) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    let omega = 2.0 * std::f64::consts::PI * freq_hz / sample_rate as f64;
    let coeff = 2.0 * omega.cos();
    let mut q1 = 0.0f64;
    let mut q2 = 0.0f64;

    for sample in samples {
        let q0 = coeff * q1 - q2 + f64::from(*sample);
        q2 = q1;
        q1 = q0;
    }

    let real = q1 - q2 * omega.cos();
    let imag = q2 * omega.sin();
    (real * real + imag * imag).sqrt()
}

fn max_windowed_goertzel_magnitude(
    samples: &[f32],
    start: usize,
    end: usize,
    freq_hz: f64,
    sample_rate: usize,
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
