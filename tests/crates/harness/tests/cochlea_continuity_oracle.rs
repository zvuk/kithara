#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use cochlea_features::{Audio as CochleaAudio, SegmentOpts, segment_timeline};
use kithara::{
    audio::mock::TestPcmReader,
    platform::time::{Duration, WallInstant},
    signal::AudioSpec,
    warp::{StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    cochlea::{marked_rhythm_markers, synchronization_failures},
    offline::{OfflinePlayer, OfflinePlayerOptions, append_source_loaded, resource_from_reader},
};
use kithara_test_fixtures::integration_fixtures::constant_half;
use num_traits::AsPrimitive;

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn render_tempo_control(
    sample_rate: u32,
    played_bpm: f64,
    decorated: bool,
    live_rate: bool,
) -> Vec<f32> {
    let period = f64::from(sample_rate) * 0.5;
    let sample_rate_frames: usize = sample_rate.as_();
    let frames = sample_rate_frames * 24;
    let source = (0..frames)
        .map(|frame| {
            let frame: f64 = frame.as_();
            let beat = frame / period;
            let phase = beat.fract();
            let ordinal: usize = beat.floor().as_();
            let level = if ordinal.is_multiple_of(4) { 0.9 } else { 0.7 };
            if decorated && ordinal.is_multiple_of(3) && (0.3..0.306).contains(&phase) {
                0.65
            } else if phase < 0.12 {
                (level * (1.0 - phase / 0.12)).as_()
            } else {
                0.0
            }
        })
        .collect();
    let spec = AudioSpec::new(
        CHANNELS,
        NonZeroU32::new(sample_rate).expect("control rate"),
    );
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .block_on_underrun(true)
            .warp(WarpConfig::builder().backend(StretchKind::Glide).build())
            .build(),
        sample_rate,
    )
    .await;
    let _ = harness.render(BLOCK_FRAMES).await;
    let deck =
        harness.pcm_deck(resource_from_reader(TestPcmReader::with_samples(spec, source)).into());
    let id = harness
        .with_queue(move |player| {
            if !live_rate {
                player
                    .set_default_rate((played_bpm / 120.0).as_())
                    .expect("control playback rate");
            }
            let id = player.append(deck).expect("append tempo control");
            player
                .select(id, kithara::queue::Transition::None)
                .expect("select tempo control");
            player.play();
            id
        })
        .await;
    harness
        .render_until_current(
            id,
            BLOCK_FRAMES,
            WallInstant::now() + Duration::from_secs(5),
        )
        .await;
    if live_rate {
        harness
            .with_queue(move |player| player.set_default_rate((played_bpm / 120.0).as_()))
            .await
            .expect("live playback rate");
    }
    hang_reset!();
    let capture_frames = sample_rate_frames * 6;
    let mut captured = Vec::with_capacity(capture_frames * usize::from(CHANNELS));
    while captured.len() < capture_frames * usize::from(CHANNELS) {
        hang_tick!(format!(
            "tempo control {played_bpm} BPM at {sample_rate}: {} frames",
            captured.len() / usize::from(CHANNELS)
        ));
        captured.extend(harness.render(BLOCK_FRAMES).await);
        let _ = harness.tick_and_drain().await;
        hang_reset!();
    }
    captured.truncate(capture_frames * usize::from(CHANNELS));
    harness.close().await;
    captured
}

#[kithara::test(tokio, multi_thread)]
#[case::correct_48k(48_000, 127.0, true)]
#[case::correct_44k(44_100, 127.0, true)]
#[case::fast_48k(48_000, 128.0, false)]
#[case::fast_44k(44_100, 128.0, false)]
#[case::slow_48k(48_000, 126.0, false)]
#[case::slow_44k(44_100, 126.0, false)]
#[case::half_48k(48_000, 63.5, false)]
#[case::half_44k(44_100, 63.5, false)]
#[case::double_48k(48_000, 254.0, false)]
#[case::double_44k(44_100, 254.0, false)]
#[case::outside_fast_48k(48_000, 127.51, false)]
#[case::outside_fast_44k(44_100, 127.51, false)]
#[case::outside_slow_48k(48_000, 126.49, false)]
#[case::outside_slow_44k(44_100, 126.49, false)]
async fn tempo_oracle_distinguishes_the_rendered_rate(
    #[case] sample_rate: u32,
    #[case] played_bpm: f64,
    #[case] accepted: bool,
) {
    let capture = render_tempo_control(sample_rate, played_bpm, false, false).await;
    let (markers, _) = marked_rhythm_markers(&capture, CHANNELS, sample_rate);
    assert!(
        markers.len() >= 3,
        "rate control must contain measurable pulses: {markers:?}"
    );
    let failures =
        synchronization_failures("tempo control", &[&capture], CHANNELS, sample_rate, 127.0);
    assert_eq!(
        failures.is_empty(),
        accepted,
        "{played_bpm} BPM at {sample_rate}: {failures:?}"
    );
    if !accepted {
        assert!(
            failures.iter().any(|failure| failure.contains("tempo")),
            "wrong-rate control must fail its tempo measurement: {failures:?}"
        );
    }
}

#[kithara::test(tokio, multi_thread)]
#[case::correct_48k(48_000, 127.0, true)]
#[case::correct_44k(44_100, 127.0, true)]
#[case::fast_48k(48_000, 128.0, false)]
#[case::fast_44k(44_100, 128.0, false)]
#[case::slow_48k(48_000, 126.0, false)]
#[case::slow_44k(44_100, 126.0, false)]
#[case::outside_fast_48k(48_000, 127.51, false)]
#[case::outside_fast_44k(44_100, 127.51, false)]
#[case::outside_slow_48k(48_000, 126.49, false)]
#[case::outside_slow_44k(44_100, 126.49, false)]
#[case::half_48k(48_000, 63.5, false)]
#[case::half_44k(44_100, 63.5, false)]
#[case::double_48k(48_000, 254.0, false)]
#[case::double_44k(44_100, 254.0, false)]
async fn tempo_oracle_distinguishes_a_decorated_capture(
    #[case] sample_rate: u32,
    #[case] played_bpm: f64,
    #[case] accepted: bool,
) {
    let capture = render_tempo_control(sample_rate, played_bpm, true, false).await;
    let failures = synchronization_failures(
        "decorated tempo control",
        &[&capture],
        CHANNELS,
        sample_rate,
        127.0,
    );
    assert_eq!(
        failures.is_empty(),
        accepted,
        "{played_bpm} BPM at {sample_rate}: {failures:?}"
    );
    if !accepted {
        assert!(
            failures.iter().any(|failure| failure.contains("tempo")),
            "wrong-rate decorated control must fail its tempo measurement: {failures:?}"
        );
    }
}

#[kithara::test(tokio, multi_thread)]
#[case::cd_rate(44_100)]
#[case::render_rate(48_000)]
async fn tempo_oracle_preserves_the_live_rate_boundary(#[case] sample_rate: u32) {
    let capture = render_tempo_control(sample_rate, 127.0, false, true).await;
    let failures = synchronization_failures("live rate", &[&capture], CHANNELS, sample_rate, 127.0);
    assert!(failures.is_empty(), "{failures:?}");
}

#[kithara::test(tokio, multi_thread)]
#[case::cd_rate(44_100)]
#[case::render_rate(48_000)]
async fn decorated_render_rejects_a_one_frame_phase_error(#[case] sample_rate: u32) {
    let capture = render_tempo_control(sample_rate, 127.0, true, false).await;
    let shifted = [vec![0.0; usize::from(CHANNELS)], capture.clone()].concat();
    let failures = synchronization_failures(
        "one frame",
        &[&capture, &shifted],
        CHANNELS,
        sample_rate,
        127.0,
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("beat phase spread is 1 frame")),
        "{failures:?}"
    );
}

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const CAPTURE_FRAMES: usize = SAMPLE_RATE as usize * 2;
const SEAM_FRAME: usize = SAMPLE_RATE as usize;
const PAUSE_FRAMES: usize = SAMPLE_RATE as usize * 3 / 50;
const SEGMENT_WINDOW_MS: f64 = 20.0;
const SAMPLE_SILENCE_THRESHOLD: f32 = 1.0e-4;

#[kithara_test_utils::kithara::hang_watchdog]
async fn render_no_switch_control(constant_half: &'static [u8]) -> Vec<f32> {
    let spec = AudioSpec::new(
        CHANNELS,
        NonZeroU32::new(SAMPLE_RATE).expect("test sample rate is non-zero"),
    );
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let deck_source = harness
        .pcm_deck((resource_from_reader(TestPcmReader::with_pcm(spec, 3.0, constant_half))).into());
    let deck_id = append_source_loaded(&harness, harness.player(), deck_source).await;
    harness
        .with_queue(move |player| {
            player
                .select(deck_id, kithara::queue::Transition::None)
                .expect("select no-switch control item");
            player.play();
        })
        .await;

    hang_reset!();
    let _real_io = kithara::platform::flash::real_io();
    let mut rendered = Vec::with_capacity(CAPTURE_FRAMES * usize::from(CHANNELS));
    while rendered.len() / usize::from(CHANNELS) < CAPTURE_FRAMES {
        hang_tick!();
        rendered.extend(harness.render(BLOCK_FRAMES).await);
        let _ = harness.tick_and_drain().await;
        hang_reset!();
    }
    rendered.truncate(CAPTURE_FRAMES * usize::from(CHANNELS));
    harness.close().await;
    rendered
}

fn max_sample_error(
    candidate: &[f32],
    control: &[f32],
    start_frame: usize,
    end_frame: usize,
) -> f32 {
    assert_eq!(candidate.len(), control.len(), "comparison PCM lengths");
    let channels = usize::from(CHANNELS);
    candidate[start_frame * channels..end_frame * channels]
        .iter()
        .zip(&control[start_frame * channels..end_frame * channels])
        .map(|(candidate, control)| (candidate - control).abs())
        .fold(0.0, f32::max)
}

fn longest_silent_run(samples: &[f32], start_frame: usize, end_frame: usize) -> usize {
    let channels = usize::from(CHANNELS);
    let mut current = 0usize;
    let mut longest = 0usize;
    for frame in samples[start_frame * channels..end_frame * channels].chunks_exact(channels) {
        if frame
            .iter()
            .all(|sample| sample.abs() <= SAMPLE_SILENCE_THRESHOLD)
        {
            current = current.saturating_add(1);
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn cochlea_silent_segments(samples: &[f32], start_frame: usize, end_frame: usize) -> usize {
    let audio = CochleaAudio {
        samples: samples.to_vec(),
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
    };
    let timeline = segment_timeline(
        &audio,
        &SegmentOpts::default().with_window_ms(SEGMENT_WINDOW_MS),
    );
    let frame_ms = 1_000.0 / f64::from(SAMPLE_RATE);
    let (start_frame, end_frame): (f64, f64) = (start_frame.as_(), end_frame.as_());
    let (start_ms, end_ms) = (start_frame * frame_ms, end_frame * frame_ms);
    timeline
        .segments
        .iter()
        .filter(|segment| {
            segment.start_ms >= start_ms && segment.end_ms <= end_ms && segment.silent
        })
        .count()
}

#[kithara::test(tokio)]
async fn cochlea_oracle_rejects_click_and_pause_in_rendered_no_switch_control(
    constant_half: &'static [u8],
) {
    let control = render_no_switch_control(constant_half).await;
    let pause_end = SEAM_FRAME + PAUSE_FRAMES;
    let channels = usize::from(CHANNELS);

    assert!(
        control[SEAM_FRAME * channels..pause_end * channels]
            .iter()
            .all(|sample| sample.abs() > SAMPLE_SILENCE_THRESHOLD),
        "no-switch control must be audible throughout the seam window"
    );
    assert_eq!(
        longest_silent_run(&control, SEAM_FRAME, pause_end),
        0,
        "no-switch control must not contain an interior pause"
    );
    assert_eq!(
        cochlea_silent_segments(&control, SEAM_FRAME, pause_end),
        0,
        "Cochlea must see no silent segment in the no-switch control"
    );

    let mut clicked = control.clone();
    let click_frame = SEAM_FRAME + 17;
    let click_level = control[click_frame * channels..(click_frame + 1) * channels]
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0, f32::max);
    for sample in &mut clicked[click_frame * channels..(click_frame + 1) * channels] {
        *sample = -*sample;
    }
    let click_error = max_sample_error(&clicked, &control, SEAM_FRAME, pause_end);
    assert!(
        (click_error - 2.0 * click_level).abs() <= f32::EPSILON,
        "PCM oracle must measure the full injected one-sample click; expected={}, measured={click_error}",
        2.0 * click_level,
    );

    let mut paused = control.clone();
    paused[SEAM_FRAME * channels..pause_end * channels].fill(0.0);
    assert!(
        longest_silent_run(&paused, SEAM_FRAME, pause_end) >= PAUSE_FRAMES,
        "PCM oracle must reject an injected {PAUSE_FRAMES}-frame pause"
    );
    assert!(
        cochlea_silent_segments(&paused, SEAM_FRAME, pause_end) >= 3,
        "Cochlea must reject the injected interior pause with silent timeline segments"
    );
}
