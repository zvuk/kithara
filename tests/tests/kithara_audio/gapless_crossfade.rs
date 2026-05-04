use kithara::{
    decode::{GaplessInfo, GaplessTrimmer, PcmChunk, PcmMeta, PcmSpec},
    platform::time::Duration,
};
use kithara_bufpool::pcm_pool;
use kithara_test_utils::signal_pcm::signal::{SignalFn, SineWave};

use crate::gapless_common::{
    AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
};

const TRACK_FRAMES: usize = 48_000;
const SINE_FREQ_HZ: f64 = 1_000.0;

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn synthetic_gapless_tracks_have_no_boundary_energy_dip() {
    let spec = PcmSpec {
        channels: GAPLESS_CHANNELS,
        sample_rate: GAPLESS_SAMPLE_RATE,
    };
    let leading_frames = usize::try_from(AAC_GAPLESS_ENCODER_DELAY).expect("AAC delay fits usize");
    let trailing_frames =
        usize::try_from(AAC_GAPLESS_TRAILING_DELAY).expect("AAC padding fits usize");

    let first = trim_track(
        padded_sine_track(0, TRACK_FRAMES, leading_frames, trailing_frames, spec),
        spec,
        leading_frames,
        trailing_frames,
    );
    let second = trim_track(
        padded_sine_track(
            TRACK_FRAMES,
            TRACK_FRAMES,
            leading_frames,
            trailing_frames,
            spec,
        ),
        spec,
        leading_frames,
        trailing_frames,
    );

    let mut joined = first;
    joined.extend_from_slice(&second);

    let expected = sine_samples(0, TRACK_FRAMES * 2, spec);
    assert_samples_close(&joined, &expected);

    let max_dip_db = boundary_energy_dip_db(
        &joined,
        usize::from(GAPLESS_CHANNELS),
        usize::try_from(GAPLESS_SAMPLE_RATE).expect("sample rate fits usize"),
        TRACK_FRAMES,
        5,
    );

    assert!(
        max_dip_db < 1.5,
        "gapless boundary energy dip must stay below 1.5 dB, got {max_dip_db:.3} dB"
    );
}

fn padded_sine_track(
    start_frame: usize,
    visible_frames: usize,
    leading_frames: usize,
    trailing_frames: usize,
    spec: PcmSpec,
) -> Vec<f32> {
    let channels = usize::from(spec.channels);
    let mut samples = Vec::with_capacity(
        leading_frames
            .saturating_add(visible_frames)
            .saturating_add(trailing_frames)
            .saturating_mul(channels),
    );
    samples.resize(leading_frames.saturating_mul(channels), 0.0);
    samples.extend(sine_samples(start_frame, visible_frames, spec));
    samples.resize(
        samples
            .len()
            .saturating_add(trailing_frames.saturating_mul(channels)),
        0.0,
    );
    samples
}

fn trim_track(
    samples: Vec<f32>,
    spec: PcmSpec,
    leading_frames: usize,
    trailing_frames: usize,
) -> Vec<f32> {
    let mut gapless = GaplessInfo::default();
    gapless.leading_frames = u64::try_from(leading_frames).expect("leading frames fit u64");
    gapless.trailing_frames = u64::try_from(trailing_frames).expect("trailing frames fit u64");
    let mut trimmer = GaplessTrimmer::from_info(gapless);
    let chunk = PcmChunk::new(
        PcmMeta {
            spec,
            ..Default::default()
        },
        pcm_pool().attach(samples),
    );

    let mut output = Vec::new();
    for chunk in trimmer.push(chunk) {
        output.extend_from_slice(chunk.samples());
    }
    for chunk in trimmer.flush() {
        output.extend_from_slice(chunk.samples());
    }
    output
}

fn sine_samples(start_frame: usize, frames: usize, spec: PcmSpec) -> Vec<f32> {
    let channels = usize::from(spec.channels);
    let mut samples = Vec::with_capacity(frames.saturating_mul(channels));
    let signal = SineWave(SINE_FREQ_HZ);
    for frame in start_frame..start_frame.saturating_add(frames) {
        let sample = f32::from(signal.sample(frame, spec.sample_rate)) / f32::from(i16::MAX);
        for _ in 0..channels {
            samples.push(sample);
        }
    }
    samples
}

fn assert_samples_close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (idx, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        let delta = (actual - expected).abs();
        assert!(
            delta <= 1.0e-6,
            "sample {idx}: expected {expected}, got {actual} (delta {delta})"
        );
    }
}

fn boundary_energy_dip_db(
    samples: &[f32],
    channels: usize,
    sample_rate: usize,
    boundary_frame: usize,
    window_ms: usize,
) -> f64 {
    let window_frames = sample_rate.saturating_mul(window_ms).div_ceil(1_000);
    let left_start = boundary_frame.saturating_sub(window_frames);
    let right_end = boundary_frame.saturating_add(window_frames);
    let center_half = window_frames / 2;
    let center_start = boundary_frame.saturating_sub(center_half);
    let center_end = boundary_frame.saturating_add(center_half);

    let left = rms(samples, channels, left_start, boundary_frame);
    let right = rms(samples, channels, boundary_frame, right_end);
    let center = rms(samples, channels, center_start, center_end);
    let reference = left.max(right);
    if reference <= f64::EPSILON || center >= reference {
        return 0.0;
    }

    20.0 * (reference / center.max(f64::EPSILON)).log10()
}

fn rms(samples: &[f32], channels: usize, start_frame: usize, end_frame: usize) -> f64 {
    let start = start_frame.saturating_mul(channels).min(samples.len());
    let end = end_frame.saturating_mul(channels).min(samples.len());
    if end <= start {
        return 0.0;
    }

    let sum = samples[start..end]
        .iter()
        .map(|sample| {
            let sample = f64::from(*sample);
            sample * sample
        })
        .sum::<f64>();
    let sample_count = u32::try_from(end - start).expect("RMS window sample count fits u32");
    (sum / f64::from(sample_count)).sqrt()
}
