#![cfg(not(target_arch = "wasm32"))]

use std::{num::NonZeroU32, sync::Arc};

use kithara_assets::StoreOptions;
use kithara_decode::{GaplessMode, SilenceTrimParams};
use kithara_encode::codec::AudioCodec;
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_play::{PlayerConfig, PlayerEvent, Resource, ResourceConfig};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{
        GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
    },
    temp_dir,
};

use super::offline_player_harness::OfflinePlayerHarness;
use crate::gapless_common::{
    AAC_FRAME_SAMPLES, AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_FRAMES,
    AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_SEGMENTS, AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS,
    GAPLESS_SAMPLE_RATE,
};

const BLOCK_FRAMES: usize = 512;
const POST_ROLL_BLOCKS: usize = 8;
const SILENCE_THRESHOLD: f32 = 1.0e-3;
/// Phase-locked tone: period = 100 frames at 48 kHz, integer divisor of the
/// sample rate, integer divisor of the AAC frame size (1024 / 100 ≠ int but
/// the wave is still perfectly periodic over any block large enough — only
/// the segment-boundary alignment matters here).
const SINE_HZ: f64 = 480.0;

fn silence_trim_with_trailing() -> GaplessMode {
    GaplessMode::SilenceTrim(SilenceTrimParams {
        trim_trailing: true,
        ..Default::default()
    })
}

fn expected_visible_frames(encoder_delay: u32, trailing_delay: u32) -> usize {
    let packets_per_segment = AAC_GAPLESS_SEGMENT_FRAMES.div_ceil(AAC_FRAME_SAMPLES);
    let native_encoder_delay = AAC_FRAME_SAMPLES;
    packets_per_segment
        .saturating_mul(AAC_FRAME_SAMPLES)
        .saturating_mul(AAC_GAPLESS_SEGMENTS)
        .saturating_sub(native_encoder_delay)
        .saturating_sub(usize::try_from(encoder_delay).expect("encoder delay fits usize"))
        .saturating_sub(usize::try_from(trailing_delay).expect("trailing delay fits usize"))
}

fn expected_total_decoded_frames() -> usize {
    let packets_per_segment = AAC_GAPLESS_SEGMENT_FRAMES.div_ceil(AAC_FRAME_SAMPLES);
    let native_encoder_delay = AAC_FRAME_SAMPLES;
    packets_per_segment
        .saturating_mul(AAC_FRAME_SAMPLES)
        .saturating_mul(AAC_GAPLESS_SEGMENTS)
        .saturating_sub(native_encoder_delay)
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn single_track_silence_trim_strips_leading_priming(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        GAPLESS_SAMPLE_RATE,
    );

    let resource = create_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "single-leading",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        None,
        0,
    )
    .await;

    load_tagged_queue(&harness, [resource]);

    let (rendered, events) = render_until_item_end(&harness, "single-leading").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                if id.as_ref() == "single-leading"
        )
    })
    .expect("ItemDidPlayToEnd must fire for the single-track fixture");

    let audio_end = audio_end_frame(&left, 0.05);
    let visible = expected_visible_frames(AAC_GAPLESS_ENCODER_DELAY, 0);
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");
    assert_close_to(
        audio_end,
        visible,
        sample_rate / 10,
        "rendered length must approximate visible (no leading priming)",
        &events,
    );

    let head_rms = rms(&left, 0, AAC_FRAME_SAMPLES.min(left.len()));
    assert!(
        head_rms > 0.2,
        "head RMS too low — leading priming likely not trimmed: head_rms={head_rms:.4}, \
         events={events:?}"
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn two_tracks_gapless_no_click_with_silence_trim_zero_crossfade(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let visible = expected_visible_frames(AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_TRAILING_DELAY);
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        GAPLESS_SAMPLE_RATE,
    );

    let first = create_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-1",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        0,
    )
    .await;
    let second = create_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-2",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        u64::try_from(visible).expect("visible frames fit u64"),
    )
    .await;

    load_tagged_queue(&harness, [first, second]);

    let (rendered, events) = render_until_item_end(&harness, "item-2").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");

    let item1_end = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                if id.as_ref() == "item-1"
        )
    })
    .expect("first item must emit ItemDidPlayToEnd");
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                if id.as_ref() == "item-2"
        )
    })
    .expect("second item must emit ItemDidPlayToEnd");

    let joined_expected = visible.saturating_mul(2);
    let audio_end = audio_end_frame(&left, 0.05);
    assert_close_to(
        audio_end,
        joined_expected,
        sample_rate / 4,
        "joined rendered length must approximate 2 × visible frames",
        &events,
    );

    let search_start = item1_end.saturating_sub(BLOCK_FRAMES * 2);
    let search_end = item1_end.saturating_add(BLOCK_FRAMES * 2).min(left.len());
    let max_silence = max_silence_run(&left, search_start, search_end);
    assert!(
        max_silence < BLOCK_FRAMES,
        "boundary silence run must stay below one render block ({BLOCK_FRAMES} frames), \
         got {max_silence} frames in [{search_start}..{search_end}); events={events:?}",
    );

    if item1_end > 0 && item1_end < left.len() {
        let step = (left[item1_end] - left[item1_end - 1]).abs();
        assert!(
            step < 0.5,
            "boundary discontinuity {step:.3} too large — likely an audible click; \
             item1_end={item1_end}, events={events:?}"
        );
    }
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn disabled_gapless_mode_keeps_full_decoded_length(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .crossfade_duration(0.0)
            .gapless_mode(GaplessMode::Disabled)
            .build(),
        GAPLESS_SAMPLE_RATE,
    );

    let resource = create_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "disabled",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        0,
    )
    .await;

    load_tagged_queue(&harness, [resource]);

    let (rendered, events) = render_until_item_end(&harness, "disabled").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                if id.as_ref() == "disabled"
        )
    })
    .expect("ItemDidPlayToEnd must fire");

    let audio_end = audio_end_frame(&left, 0.05);
    let total_decoded = expected_total_decoded_frames();
    let sample_rate = usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize");
    assert_close_to(
        audio_end,
        total_decoded,
        sample_rate / 10,
        "Disabled mode must pass through full decoded PCM (no leading/trailing trim)",
        &events,
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn single_track_silence_trim_heuristic_strips_leading_when_no_gapless_metadata(
    temp_dir: TestTempDir,
) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        GAPLESS_SAMPLE_RATE,
    );

    let resource = create_resource_with_encoding(
        harness.player(),
        &server,
        temp_dir.path(),
        "heuristic-leading",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        None,
        0,
        GaplessEncoding::None,
    )
    .await;

    load_tagged_queue(&harness, [resource]);

    let (rendered, events) = render_until_item_end(&harness, "heuristic-leading").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));

    let head_rms = rms(&left, 0, AAC_FRAME_SAMPLES.min(left.len()));
    assert!(
        head_rms > 0.2,
        "head RMS too low — heuristic SilenceTrim did not strip leading silence: \
         head_rms={head_rms:.4}, events={events:?}"
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn two_tracks_silence_trim_heuristic_no_click_when_no_gapless_metadata(
    temp_dir: TestTempDir,
) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        GAPLESS_SAMPLE_RATE,
    );

    let visible = expected_visible_frames(AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_TRAILING_DELAY);

    let first = create_resource_with_encoding(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-1",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        0,
        GaplessEncoding::None,
    )
    .await;
    let second = create_resource_with_encoding(
        harness.player(),
        &server,
        temp_dir.path(),
        "item-2",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        u64::try_from(visible).expect("visible frames fit u64"),
        GaplessEncoding::None,
    )
    .await;

    load_tagged_queue(&harness, [first, second]);

    let (rendered, events) = render_until_item_end(&harness, "item-2").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));

    let item1_end = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                if id.as_ref() == "item-1"
        )
    })
    .expect("first item must emit ItemDidPlayToEnd");

    let search_start = item1_end.saturating_sub(BLOCK_FRAMES * 2);
    let search_end = item1_end.saturating_add(BLOCK_FRAMES * 2).min(left.len());
    let max_silence = max_silence_run(&left, search_start, search_end);
    assert!(
        max_silence < BLOCK_FRAMES,
        "boundary silence run must stay below one render block ({BLOCK_FRAMES} frames), \
         got {max_silence} frames in [{search_start}..{search_end}); events={events:?}",
    );

    if item1_end > 0 && item1_end < left.len() {
        let step = (left[item1_end] - left[item1_end - 1]).abs();
        assert!(
            step < 0.5,
            "boundary discontinuity {step:.3} too large with heuristic SilenceTrim; \
             item1_end={item1_end}, events={events:?}"
        );
    }

    let win = (GAPLESS_SAMPLE_RATE as usize) / 100;
    let probe_lookback = (GAPLESS_SAMPLE_RATE as usize) / 20;
    if item1_end >= probe_lookback && item1_end + probe_lookback + win <= left.len() {
        let mag_body_before = goertzel_magnitude(
            &left[item1_end - probe_lookback..item1_end - probe_lookback + win],
            SINE_HZ,
            GAPLESS_SAMPLE_RATE as usize,
        );
        let mag_body_after = goertzel_magnitude(
            &left[item1_end + probe_lookback..item1_end + probe_lookback + win],
            SINE_HZ,
            GAPLESS_SAMPLE_RATE as usize,
        );
        let ratio = (mag_body_after / mag_body_before.max(f64::MIN_POSITIVE))
            .max(mag_body_before / mag_body_after.max(f64::MIN_POSITIVE));
        assert!(
            ratio < 1.5,
            "body-level magnitude differs too much across the boundary \
             (heuristic ate audible content somewhere): \
             before={mag_body_before:.2}, after={mag_body_after:.2}, ratio={ratio:.2}; \
             events={events:?}"
        );
    }
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn single_track_silence_trim_heuristic_fade_out_smooths_trailing_edge(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        GAPLESS_SAMPLE_RATE,
    );

    let resource = create_resource_with_encoding(
        harness.player(),
        &server,
        temp_dir.path(),
        "fade-out-edge",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        0,
        GaplessEncoding::None,
    )
    .await;
    load_tagged_queue(&harness, [resource]);

    let (rendered, events) = render_until_item_end(&harness, "fade-out-edge").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));

    let item_end = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                if id.as_ref() == "fade-out-edge"
        )
    })
    .expect("ItemDidPlayToEnd must fire");

    let body_window = AAC_FRAME_SAMPLES * 4;
    let body_end = item_end.saturating_sub(GAPLESS_SAMPLE_RATE as usize / 100);
    let body_start = body_end.saturating_sub(body_window);
    let body_rms = rms(&left, body_start, body_end);
    assert!(
        body_rms > 0.2,
        "body RMS unexpectedly low — heuristic likely chewed audible content: \
         body_rms={body_rms:.4}"
    );

    let tail_window = (GAPLESS_SAMPLE_RATE as usize) / 1000;
    let tail_start = item_end.saturating_sub(tail_window);
    let tail_end = item_end.min(left.len());
    let tail_peak = left[tail_start..tail_end]
        .iter()
        .map(|s| s.abs())
        .fold(0.0_f32, f32::max);
    let body_peak = left[body_start..body_end]
        .iter()
        .map(|s| s.abs())
        .fold(0.0_f32, f32::max);
    assert!(
        tail_peak < body_peak * 0.6,
        "trailing fade-out did not attenuate the edge: \
         tail_peak={tail_peak:.4}, body_peak={body_peak:.4}, ratio={:.2}",
        tail_peak / body_peak.max(f32::MIN_POSITIVE),
    );
}

#[expect(
    clippy::cast_precision_loss,
    reason = "test-only spectral probe: sample_rate fits f64 precision well below 2^52"
)]
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

async fn create_resource(
    player: &kithara_play::PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &std::path::Path,
    item_id: &'static str,
    encoder_delay: Option<u32>,
    trailing_delay: Option<u32>,
    start_frame: u64,
) -> (Resource, Arc<str>) {
    create_resource_with_encoding(
        player,
        server,
        cache_dir,
        item_id,
        encoder_delay,
        trailing_delay,
        start_frame,
        GaplessEncoding::default(),
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "fixture builder: each parameter pins one HLS-fixture knob"
)]
async fn create_resource_with_encoding(
    player: &kithara_play::PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &std::path::Path,
    item_id: &'static str,
    encoder_delay: Option<u32>,
    trailing_delay: Option<u32>,
    start_frame: u64,
    gapless_encoding: GaplessEncoding,
) -> (Resource, Arc<str>) {
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: NonZeroU32::new(
                        u32::try_from(start_frame).expect("start_frame fits u32"),
                    ),
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: encoder_delay.and_then(NonZeroU32::new),
                    trailing_delay: trailing_delay.and_then(NonZeroU32::new),
                    source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: SINE_HZ }),
                    gapless_encoding,
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create gapless e2e HLS fixture");

    let item_id = Arc::<str>::from(item_id);
    let store = StoreOptions::new(cache_dir);
    let mut config = ResourceConfig::for_src(created.master_url().as_str())
        .expect("valid HLS master URL")
        .store(store)
        .build();
    config = player.prepare_config(config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS resource for gapless e2e fixture");
    resource.preload().await;
    (resource, item_id)
}

fn load_tagged_queue<const N: usize>(
    harness: &OfflinePlayerHarness,
    items: [(Resource, Arc<str>); N],
) {
    harness.player().reserve_slots(items.len());
    for (index, (resource, item_id)) in items.into_iter().enumerate() {
        harness
            .player()
            .replace_item_tagged(index, resource, Some(item_id));
    }
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");
}

async fn render_until_item_end(
    harness: &OfflinePlayerHarness,
    terminal_item_id: &'static str,
) -> (Vec<f32>, Vec<TimedPlayerEvent>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut rendered = Vec::new();
    let mut rendered_frames = 0usize;
    let mut events = Vec::new();

    loop {
        let block = harness.render(BLOCK_FRAMES);
        rendered.extend_from_slice(&block);
        rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES);
        events.extend(
            harness
                .tick_and_drain()
                .into_iter()
                .map(|event| TimedPlayerEvent {
                    frame_end: rendered_frames,
                    event,
                }),
        );

        if events.iter().any(|event| {
            matches!(
                &event.event,
                PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                    if id.as_ref() == terminal_item_id
            )
        }) {
            for _ in 0..POST_ROLL_BLOCKS {
                let block = harness.render(BLOCK_FRAMES);
                rendered.extend_from_slice(&block);
                rendered_frames = rendered_frames.saturating_add(BLOCK_FRAMES);
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
            "timed out waiting for {terminal_item_id} to finish; events={events:?}"
        );
        sleep(Duration::from_millis(5)).await;
    }
}

fn deinterleave_left(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame[0])
        .collect()
}

#[derive(Clone, Debug)]
struct TimedPlayerEvent {
    frame_end: usize,
    event: PlayerEvent,
}

fn timed_event_frame_end<P>(events: &[TimedPlayerEvent], predicate: P) -> Option<usize>
where
    P: Fn(&PlayerEvent) -> bool,
{
    events
        .iter()
        .find(|timed| predicate(&timed.event))
        .map(|timed| timed.frame_end)
}

/// Index just past the last sample whose absolute amplitude is at or above
/// `threshold`. Used to anchor "rendered length" on the actual audible PCM
/// region rather than on the `ItemDidPlayToEnd` event observation frame —
/// the latter trails the audio thread by ~100 ms in the offline harness.
fn audio_end_frame(samples: &[f32], threshold: f32) -> usize {
    samples
        .iter()
        .rposition(|sample| sample.abs() >= threshold)
        .map_or(0, |idx| idx + 1)
}

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

fn rms(samples: &[f32], start: usize, end: usize) -> f32 {
    let end = end.min(samples.len());
    if end <= start {
        return 0.0;
    }
    let slice = &samples[start..end];
    let sum_sq: f64 = slice.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "test-only RMS: slice length fits f64; result narrowed to f32 to match sample type"
    )]
    let rms = (sum_sq / slice.len() as f64).sqrt() as f32;
    rms
}

fn assert_close_to(
    actual: usize,
    expected: usize,
    tolerance: usize,
    label: &str,
    events: &[TimedPlayerEvent],
) {
    let delta = actual.abs_diff(expected);
    assert!(
        delta <= tolerance,
        "{label}: actual={actual}, expected={expected}, delta={delta}, tolerance={tolerance}; \
         events={events:?}"
    );
}
