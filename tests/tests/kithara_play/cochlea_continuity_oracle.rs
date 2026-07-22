#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use cochlea_features::{Audio as CochleaAudio, SegmentOpts, segment_timeline};
use kithara::decode::PcmSpec;
use kithara_integration_tests::{audio_mock::TestPcmReader, offline::resource_from_reader};

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const CAPTURE_FRAMES: usize = SAMPLE_RATE as usize * 2;
const SEAM_FRAME: usize = SAMPLE_RATE as usize;
const PAUSE_FRAMES: usize = SAMPLE_RATE as usize * 3 / 50;
const SEGMENT_WINDOW_MS: f64 = 20.0;
const SAMPLE_SILENCE_THRESHOLD: f32 = 1.0e-4;

fn render_no_switch_control() -> Vec<f32> {
    let spec = PcmSpec::new(
        CHANNELS,
        NonZeroU32::new(SAMPLE_RATE).expect("test sample rate is non-zero"),
    );
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    harness.player().insert(
        resource_from_reader(TestPcmReader::with_value(spec, 3.0, 0.5)),
        None,
        None,
    );
    harness
        .player()
        .select_item(0, true)
        .expect("select no-switch control item");

    let mut rendered = Vec::with_capacity(CAPTURE_FRAMES * usize::from(CHANNELS));
    while rendered.len() / usize::from(CHANNELS) < CAPTURE_FRAMES {
        rendered.extend(harness.render(BLOCK_FRAMES));
        let _ = harness.tick_and_drain();
    }
    rendered.truncate(CAPTURE_FRAMES * usize::from(CHANNELS));
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
    let start_ms = start_frame as f64 / f64::from(SAMPLE_RATE) * 1_000.0;
    let end_ms = end_frame as f64 / f64::from(SAMPLE_RATE) * 1_000.0;
    timeline
        .segments
        .iter()
        .filter(|segment| {
            segment.start_ms >= start_ms && segment.end_ms <= end_ms && segment.silent
        })
        .count()
}

#[kithara::test]
fn cochlea_oracle_rejects_click_and_pause_in_rendered_no_switch_control() {
    let control = render_no_switch_control();
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
