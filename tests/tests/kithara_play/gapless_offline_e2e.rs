#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
use kithara::decode::DecoderBackend;
use kithara::{
    assets::StoreOptions,
    audio::{ChunkOutcome, PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome},
    bufpool::PcmPool,
    decode::{
        DecodeError, GaplessInfo, GaplessMode, GaplessTailCompensation, GaplessTrimmer, PcmChunk,
        PcmMeta, PcmSpec, SilenceTrimParams, TrackMetadata,
    },
    events::EventBus,
    platform::{
        sync::Arc,
        time::{self, Duration, Instant},
    },
    play::{PlayerConfig, PlayerEvent, Resource, ResourceConfig},
    stream::AudioCodec,
};
use kithara_integration_tests::{
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
const FUSED_FIXTURE_SOURCE_RATE: u32 = 44_100;
const FUSED_FIXTURE_DEVICE_RATE: u32 = 48_000;
const FUSED_FIXTURE_SOURCE_FRAMES: u64 = 44_100;
const FUSED_FIXTURE_IDEAL_DEVICE_FRAMES: usize = 48_000;
const FUSED_FIXTURE_SEAM_OMEGA: f64 = 0.23925;
const FUSED_FIXTURE_SEAM_PHASE: f64 = -1.365_523_678_408_751_2;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
const APPLE_FUSED_DEFICIT_SOURCE_FRAMES: u64 = 261_120;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
const APPLE_FUSED_DEFICIT_SEGMENT_SECS: f64 = 5.921_088_435_374_15;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
const APPLE_FUSED_DEFICIT_SEGMENTS: usize = 1;
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
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
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
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
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
async fn two_tracks_gapless_stitch_continuity_metric(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let stitch_frame = crate::gapless_common::generated_aac_elst_visible_frames();
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        GAPLESS_SAMPLE_RATE,
    );

    let first = create_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "continuity-1",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        0,
    )
    .await;
    let second = create_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        "continuity-2",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        u64::try_from(stitch_frame).expect("stitch frame fits u64"),
    )
    .await;

    load_tagged_queue(&harness, [first, second]);

    let (rendered, events) = render_until_item_end(&harness, "continuity-2").await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));

    assert!(
        left.len() > stitch_frame + ContinuityMetric::HALF_WINDOW_FRAMES,
        "rendered PCM must cover the stitch window; left_frames={}, stitch_frame={stitch_frame}, events={events:?}",
        left.len()
    );
    assert_prefetch_before_first_end(&events, "continuity-1");

    let switch_peak = peak_first_diff(&left, stitch_frame, ContinuityMetric::HALF_WINDOW_FRAMES);
    let (control_peak, control_count) = gapless_control_peak(&left, stitch_frame);
    assert!(
        control_count >= ContinuityMetric::MIN_CONTROL_WINDOWS,
        "gapless continuity metric needs at least {} same-track control windows, got {control_count}",
        ContinuityMetric::MIN_CONTROL_WINDOWS,
    );
    let ratio = switch_peak / control_peak.max(f32::EPSILON);
    println!(
        "GAPLESS_CONTINUITY switch_peak={switch_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < ContinuityMetric::MAX_RATIO,
        "gapless stitch discontinuity {switch_peak:.6} is {ratio:.1}x the worst same-track control window {control_peak:.6}",
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn fused_gapless_tail_compensation_restores_exact_length_at_stitch() {
    let compensated = render_synthetic_fused_deficit_seam(true).await;
    let uncompensated = render_synthetic_fused_deficit_seam(false).await;

    let stitch_frame = FUSED_FIXTURE_IDEAL_DEVICE_FRAMES;
    let compensated_db = seam_step_db(&compensated.left, stitch_frame);
    let uncompensated_db = seam_step_db(&uncompensated.left, stitch_frame - 1);
    println!(
        "FUSED_GAPLESS_DEFICIT compensated_db={compensated_db:.2} uncompensated_db={uncompensated_db:.2}"
    );

    assert_prefetch_before_first_end(&compensated.events, "fused-deficit-1");
    assert_eq!(compensated.first_frames, FUSED_FIXTURE_IDEAL_DEVICE_FRAMES);
    assert_eq!(
        uncompensated.first_frames,
        FUSED_FIXTURE_IDEAL_DEVICE_FRAMES - 1
    );
    assert!(
        compensated_db < -30.0,
        "compensated seam residual too high: {compensated_db:.2} dB; events={:?}",
        compensated.events
    );
    assert!(
        uncompensated_db > -32.0,
        "uncompensated control seam should expose the one-frame deficit: {uncompensated_db:.2} dB; events={:?}",
        uncompensated.events
    );
    assert!(
        compensated_db + 1.0 < uncompensated_db,
        "tail-side compensation must measurably improve the seam: compensated={compensated_db:.2}, uncompensated={uncompensated_db:.2}"
    );
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn apple_fused_gapless_fixture_keeps_device_rate_seam_metric(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let source_stitch_frame = APPLE_FUSED_DEFICIT_SOURCE_FRAMES;
    let expected_device_frames = usize::try_from(ceil_scaled_frames(
        source_stitch_frame,
        FUSED_FIXTURE_DEVICE_RATE,
        FUSED_FIXTURE_SOURCE_RATE,
    ))
    .expect("fixture length fits usize");
    assert_eq!(
        expected_device_frames, 284_213,
        "fixture must keep the selected one-frame-deficit search geometry"
    );

    let probe_harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        FUSED_FIXTURE_DEVICE_RATE,
    );
    let (probe, _) = create_apple_fused_resource(
        probe_harness.player(),
        &server,
        temp_dir.path(),
        "apple-fused-probe",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        None,
        0,
    )
    .await;
    let probe = drain_resource_to_eof(probe).await;
    assert_eq!(
        probe.output_frames, expected_device_frames,
        "tail-side compensation should restore exact visible device-frame length"
    );

    let pending_decision =
        render_apple_fused_deficit_seam(&server, temp_dir.path(), probe.output_frames).await;

    println!(
        "APPLE_FUSED_TAIL_SEAM measured_frames={} ideal_frames={} length_delta={} \
         seam_db={:.2} control_db={:.2} stitch_frame={} nearby={:?}",
        probe.output_frames,
        expected_device_frames,
        isize::try_from(probe.output_frames).expect("probe frames fit isize")
            - isize::try_from(expected_device_frames).expect("ideal frames fit isize"),
        pending_decision.seam_db,
        pending_decision.control_db,
        pending_decision.stitch_frame,
        pending_decision.nearby_db
    );

    assert!(pending_decision.seam_db.is_finite());
    assert!(
        pending_decision.seam_db < -26.0,
        "tail-side fused seam regression floor: {:.2} dB",
        pending_decision.seam_db
    );
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
async fn render_apple_fused_deficit_seam(
    server: &TestServerHelper,
    cache_dir: &std::path::Path,
    stitch_frame: usize,
) -> AppleFusedSeamRender {
    let source_stitch_frame = APPLE_FUSED_DEFICIT_SOURCE_FRAMES;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
            .crossfade_duration(0.0)
            .gapless_mode(silence_trim_with_trailing())
            .build(),
        FUSED_FIXTURE_DEVICE_RATE,
    );

    let (first, first_id) = create_apple_fused_resource(
        harness.player(),
        server,
        cache_dir,
        "apple-fused-1",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        None,
        0,
    )
    .await;
    let second = create_apple_fused_resource(
        harness.player(),
        server,
        cache_dir,
        "apple-fused-2",
        Some(AAC_GAPLESS_ENCODER_DELAY),
        None,
        source_stitch_frame,
    )
    .await;

    load_tagged_queue(&harness, [(first, first_id), second]);

    let control_post_roll_blocks =
        (ContinuityMetric::CONTROL_STRIDE_FRAMES / BLOCK_FRAMES) + POST_ROLL_BLOCKS;
    let (rendered, events) =
        render_until_item_end_with_post_roll(&harness, "apple-fused-1", control_post_roll_blocks)
            .await;
    let left = deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS));
    let _ = timed_event_frame_end(&events, |event| {
        matches!(
            event,
            PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                if id.as_ref() == "apple-fused-1"
        )
    })
    .expect("first fused item must emit ItemDidPlayToEnd");

    assert_prefetch_before_first_end(&events, "apple-fused-1");
    assert!(
        stitch_frame < left.len(),
        "fused stitch frame must be inside rendered PCM; stitch_frame={stitch_frame}, left_frames={}, events={events:?}",
        left.len()
    );

    let seam_db = seam_step_db(&left, stitch_frame);
    let (control_peak, control_count) = gapless_control_peak(&left, stitch_frame);
    let control_db = amplitude_db(control_peak);
    assert!(
        control_count >= ContinuityMetric::MIN_CONTROL_WINDOWS,
        "Apple fused seam metric needs at least {} same-track control windows, got {control_count}",
        ContinuityMetric::MIN_CONTROL_WINDOWS
    );
    AppleFusedSeamRender {
        control_db,
        nearby_db: nearby_seam_step_db(&left, stitch_frame),
        seam_db,
        stitch_frame,
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
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
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
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
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
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
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
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
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
    player: &kithara::play::PlayerImpl,
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
    player: &kithara::play::PlayerImpl,
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
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(PcmPool::default())
        .store(store)
        .build();
    config = player.prepare_config(config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS resource for gapless e2e fixture");
    let _ = resource.preload().await;
    (resource, item_id)
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[expect(
    clippy::too_many_arguments,
    reason = "fixture builder: each parameter pins one HLS-fixture knob"
)]
async fn create_apple_fused_resource(
    player: &kithara::play::PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &std::path::Path,
    item_id: &'static str,
    encoder_delay: Option<u32>,
    trailing_delay: Option<u32>,
    start_frame: u64,
) -> (Resource, Arc<str>) {
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(APPLE_FUSED_DEFICIT_SEGMENTS)
                .segment_duration_secs(APPLE_FUSED_DEFICIT_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: FUSED_FIXTURE_SOURCE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: NonZeroU32::new(
                        u32::try_from(start_frame).expect("start_frame fits u32"),
                    ),
                    timescale: Some(FUSED_FIXTURE_SOURCE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: encoder_delay.and_then(NonZeroU32::new),
                    trailing_delay: trailing_delay.and_then(NonZeroU32::new),
                    source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: SINE_HZ }),
                    gapless_encoding: GaplessEncoding::default(),
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create Apple fused gapless HLS fixture");

    let item_id = Arc::<str>::from(item_id);
    let store = StoreOptions::new(cache_dir);
    let config = ResourceConfig::for_src(created.master_url().as_str())
        .expect("valid HLS master URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(PcmPool::default())
        .store(store)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(DecoderBackend::Apple)
                .build(),
        )
        .build();
    let config = player.prepare_config(config);
    let mut resource = Resource::new(config)
        .await
        .expect("open HLS resource for Apple fused fixture");
    let _ = resource.preload().await;
    assert_eq!(
        resource.spec().sample_rate.get(),
        FUSED_FIXTURE_DEVICE_RATE,
        "Apple fused decoder must emit the host/device rate"
    );
    (resource, item_id)
}

async fn render_synthetic_fused_deficit_seam(tail_compensation: bool) -> SyntheticSeamRender {
    let expected_device_frames = ceil_scaled_frames(
        FUSED_FIXTURE_SOURCE_FRAMES,
        FUSED_FIXTURE_DEVICE_RATE,
        FUSED_FIXTURE_SOURCE_RATE,
    );
    assert_eq!(
        expected_device_frames,
        u64::try_from(FUSED_FIXTURE_IDEAL_DEVICE_FRAMES).expect("fixture size fits u64")
    );

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(PcmPool::default())
            .crossfade_duration(0.0)
            .gapless_mode(GaplessMode::Disabled)
            .build(),
        FUSED_FIXTURE_DEVICE_RATE,
    );
    let first_frames = synthetic_tail_trimmed_first_frames(tail_compensation);
    let first_frame_count = first_frames.len();
    let first = Resource::from_reader(
        SyntheticPcmReader::new(first_frames, first_frame_count),
        Some(Arc::from("fused-deficit-1")),
    );
    let second = Resource::from_reader(
        SyntheticPcmReader::new(
            synthetic_second_track_frames(),
            FUSED_FIXTURE_IDEAL_DEVICE_FRAMES,
        ),
        Some(Arc::from("fused-deficit-2")),
    );

    load_tagged_queue(
        &harness,
        [
            (first, Arc::from("fused-deficit-1")),
            (second, Arc::from("fused-deficit-2")),
        ],
    );

    let (rendered, events) = render_until_item_end(&harness, "fused-deficit-2").await;
    SyntheticSeamRender {
        left: deinterleave_left(&rendered, usize::from(GAPLESS_CHANNELS)),
        events,
        first_frames: first_frame_count,
    }
}

fn synthetic_tail_trimmed_first_frames(tail_compensation: bool) -> Vec<f32> {
    const TRAILING_FRAMES: u64 = 1;

    let actual_pre_trim_frames =
        FUSED_FIXTURE_IDEAL_DEVICE_FRAMES + usize::try_from(TRAILING_FRAMES).expect("fits") - 1;
    let ideal_pre_trim_frames =
        u64::try_from(actual_pre_trim_frames).expect("fixture frame count fits u64") + 1;
    let source =
        synthetic_interleaved_chunk((0..actual_pre_trim_frames).map(fused_seam_sample).collect());
    let mut trimmer = GaplessTrimmer::from(GaplessInfo::new(0, TRAILING_FRAMES))
        .with_tail_compensation(
            tail_compensation.then_some(GaplessTailCompensation::new(ideal_pre_trim_frames)),
        );

    let mut output = Vec::new();
    output.extend(left_frames_from_chunks(trimmer.push(source)));
    output.extend(left_frames_from_chunks(trimmer.flush()));
    output
}

fn synthetic_second_track_frames() -> Vec<f32> {
    let start = FUSED_FIXTURE_IDEAL_DEVICE_FRAMES;
    let end = start + FUSED_FIXTURE_IDEAL_DEVICE_FRAMES;
    (start..end).map(fused_seam_sample).collect()
}

fn synthetic_interleaved_chunk(frames: Vec<f32>) -> PcmChunk {
    let spec = PcmSpec::new(
        GAPLESS_CHANNELS,
        NonZeroU32::new(FUSED_FIXTURE_DEVICE_RATE).expect("test sample rate"),
    );
    let frame_count = frames.len();
    let sample_count = frame_count * usize::from(GAPLESS_CHANNELS);
    let mut samples = PcmPool::default().get();
    samples
        .ensure_len(sample_count)
        .expect("synthetic interleaved fixture fits PCM pool budget");
    for (frame, sample) in samples
        .chunks_exact_mut(usize::from(GAPLESS_CHANNELS))
        .zip(frames)
    {
        frame.fill(sample);
    }
    PcmChunk::new(
        PcmMeta {
            frames: u32::try_from(frame_count).expect("fixture frame count fits u32"),
            spec,
            ..Default::default()
        },
        samples,
    )
}

fn left_frames_from_chunks(chunks: impl IntoIterator<Item = PcmChunk>) -> Vec<f32> {
    chunks
        .into_iter()
        .flat_map(|chunk| {
            chunk
                .samples
                .chunks_exact(usize::from(GAPLESS_CHANNELS))
                .map(|frame| frame[0])
                .collect::<Vec<_>>()
        })
        .collect()
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

struct SyntheticSeamRender {
    left: Vec<f32>,
    events: Vec<TimedPlayerEvent>,
    first_frames: usize,
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[derive(Debug)]
struct AppleFusedSeamRender {
    control_db: f32,
    nearby_db: [f32; 7],
    seam_db: f32,
    stitch_frame: usize,
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[derive(Debug)]
struct DrainedResource {
    output_frames: usize,
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
async fn drain_resource_to_eof(mut resource: Resource) -> DrainedResource {
    let mut output_frames = 0usize;
    let mut pending = 0usize;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match resource.next_chunk().expect("drain Apple fused resource") {
            ChunkOutcome::Chunk(chunk) => {
                pending = 0;
                output_frames = output_frames.saturating_add(chunk.frames());
            }
            ChunkOutcome::Pending { .. } => {
                pending = pending.saturating_add(1);
                assert!(
                    pending < 4096,
                    "Apple fused resource stayed pending while draining to EOF"
                );
                assert!(
                    Instant::now() <= deadline,
                    "timed out draining Apple fused resource to EOF"
                );
                time::sleep(Duration::from_millis(1)).await;
            }
            ChunkOutcome::Eof { .. } => {
                return DrainedResource { output_frames };
            }
        }
    }
}

struct SyntheticPcmReader {
    bus: EventBus,
    duration_frames: usize,
    frames: Vec<f32>,
    metadata: TrackMetadata,
    position_frames: usize,
    spec: PcmSpec,
}

impl SyntheticPcmReader {
    fn new(frames: Vec<f32>, duration_frames: usize) -> Self {
        Self {
            bus: EventBus::default(),
            duration_frames,
            frames,
            metadata: TrackMetadata::default(),
            position_frames: 0,
            spec: PcmSpec::new(
                GAPLESS_CHANNELS,
                NonZeroU32::new(FUSED_FIXTURE_DEVICE_RATE).expect("test sample rate"),
            ),
        }
    }

    fn fill_planar(&mut self, output: &mut [&mut [f32]]) -> usize {
        let requested = output
            .iter()
            .map(|channel| channel.len())
            .min()
            .unwrap_or(0);
        let frames = requested.min(self.frames.len().saturating_sub(self.position_frames));
        for frame in 0..frames {
            let sample = self.frames[self.position_frames + frame];
            for channel in output.iter_mut() {
                channel[frame] = sample;
            }
        }
        self.position_frames += frames;
        frames
    }

    fn fill_interleaved(&mut self, output: &mut [f32]) -> usize {
        let channels = usize::from(self.spec.channels);
        let requested = output.len().saturating_div(channels.max(1));
        let frames = requested.min(self.frames.len().saturating_sub(self.position_frames));
        for frame in 0..frames {
            let sample = self.frames[self.position_frames + frame];
            let start = frame.saturating_mul(channels);
            let end = start.saturating_add(channels).min(output.len());
            output[start..end].fill(sample);
        }
        self.position_frames += frames;
        frames
    }

    fn read_outcome(&self, frames: usize) -> ReadOutcome {
        NonZeroUsize::new(frames).map_or_else(
            || ReadOutcome::Eof {
                position: self.position(),
            },
            |count| ReadOutcome::Frames {
                count,
                position: self.position(),
            },
        )
    }
}

impl PcmSession for SyntheticPcmReader {
    fn duration(&self) -> Option<Duration> {
        Some(duration_for_test_frames(self.duration_frames))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl PcmRead for SyntheticPcmReader {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        Ok(ChunkOutcome::Eof {
            position: self.position(),
        })
    }

    fn position(&self) -> Duration {
        duration_for_test_frames(self.position_frames)
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let frames = self.fill_interleaved(buf);
        Ok(self.read_outcome(frames))
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let frames = self.fill_planar(output);
        Ok(self.read_outcome(frames))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

impl PcmControl for SyntheticPcmReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let frames = frames_for_test_duration(position);
        if frames >= self.frames.len() {
            self.position_frames = self.frames.len();
            Ok(SeekOutcome::PastEof {
                target: position,
                duration: self.position(),
            })
        } else {
            self.position_frames = frames;
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: self.position(),
            })
        }
    }
}

async fn render_until_item_end(
    harness: &OfflinePlayerHarness,
    terminal_item_id: &'static str,
) -> (Vec<f32>, Vec<TimedPlayerEvent>) {
    render_until_item_end_with_post_roll(harness, terminal_item_id, POST_ROLL_BLOCKS).await
}

async fn render_until_item_end_with_post_roll(
    harness: &OfflinePlayerHarness,
    terminal_item_id: &'static str,
    post_roll_blocks: usize,
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
            for _ in 0..post_roll_blocks {
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
        time::sleep(Duration::from_millis(5)).await;
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

struct ContinuityMetric;

impl ContinuityMetric {
    const CONTROL_STRIDE_FRAMES: usize = GAPLESS_SAMPLE_RATE as usize / 4;
    const HALF_WINDOW_FRAMES: usize = 64;
    const MAX_RATIO: f32 = 3.0;
    const MIN_CONTROL_WINDOWS: usize = 2;
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

fn assert_prefetch_before_first_end(events: &[TimedPlayerEvent], first_item_id: &str) {
    let prefetch = events
        .iter()
        .position(|timed| matches!(&timed.event, PlayerEvent::PrefetchRequested))
        .expect("PrefetchRequested must fire so the test exercises arm_next");
    let first_end = events
        .iter()
        .position(|timed| {
            matches!(
                &timed.event,
                PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                    if id.as_ref() == first_item_id
            )
        })
        .expect("first item must emit ItemDidPlayToEnd");
    assert!(
        prefetch < first_end,
        "PrefetchRequested must precede the first ItemDidPlayToEnd; events={events:?}"
    );
}

fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
    assert!(
        (1..left.len()).contains(&center),
        "first-difference center must be in 1..{}, got {center}",
        left.len(),
    );
    let start = center.saturating_sub(half).max(1);
    let end = center.saturating_add(half).min(left.len() - 1);
    let mut peak = 0.0_f32;
    for i in start..=end {
        peak = peak.max((left[i] - left[i - 1]).abs());
    }
    peak
}

fn gapless_control_peak(left: &[f32], stitch_frame: usize) -> (f32, usize) {
    let mut peak = 0.0_f32;
    let mut count = 0usize;
    for center in [
        stitch_frame.saturating_sub(ContinuityMetric::CONTROL_STRIDE_FRAMES),
        stitch_frame.saturating_add(ContinuityMetric::CONTROL_STRIDE_FRAMES),
    ] {
        if center <= ContinuityMetric::HALF_WINDOW_FRAMES
            || center + ContinuityMetric::HALF_WINDOW_FRAMES >= left.len()
        {
            continue;
        }
        peak = peak.max(peak_first_diff(
            left,
            center,
            ContinuityMetric::HALF_WINDOW_FRAMES,
        ));
        count += 1;
    }
    (peak, count)
}

fn ceil_scaled_frames(frames: u64, output_rate: u32, input_rate: u32) -> u64 {
    let numerator = u128::from(frames).saturating_mul(u128::from(output_rate));
    let denominator = u128::from(input_rate.max(1));
    let scaled = numerator.saturating_add(denominator - 1) / denominator;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "test-only signal synthesis narrows bounded sine samples to f32"
)]
fn fused_seam_sample(global_frame: usize) -> f32 {
    let global = u32::try_from(global_frame).expect("fixture frame fits u32");
    let seam = u32::try_from(FUSED_FIXTURE_IDEAL_DEVICE_FRAMES - 1).expect("seam fits u32");
    let delta = f64::from(global) - f64::from(seam);
    (FUSED_FIXTURE_SEAM_PHASE + delta * FUSED_FIXTURE_SEAM_OMEGA).sin() as f32
}

fn seam_step_db(left: &[f32], stitch_frame: usize) -> f32 {
    assert!(
        (1..left.len()).contains(&stitch_frame),
        "stitch frame must be in 1..{}, got {stitch_frame}",
        left.len(),
    );
    amplitude_db((left[stitch_frame] - left[stitch_frame - 1]).abs())
}

fn nearby_seam_step_db(left: &[f32], stitch_frame: usize) -> [f32; 7] {
    let mut values = [0.0_f32; 7];
    let start = stitch_frame.saturating_sub(3);
    for (index, value) in values.iter_mut().enumerate() {
        *value = seam_step_db(left, start.saturating_add(index));
    }
    values
}

fn amplitude_db(value: f32) -> f32 {
    20.0 * value.max(1.0e-9).log10()
}

fn duration_for_test_frames(frames: usize) -> Duration {
    let nanos = u128::try_from(frames)
        .unwrap_or(u128::MAX)
        .saturating_mul(1_000_000_000)
        / u128::from(FUSED_FIXTURE_DEVICE_RATE);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn frames_for_test_duration(duration: Duration) -> usize {
    let frames = duration
        .as_nanos()
        .saturating_mul(u128::from(FUSED_FIXTURE_DEVICE_RATE))
        / 1_000_000_000;
    usize::try_from(frames).unwrap_or(usize::MAX)
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
