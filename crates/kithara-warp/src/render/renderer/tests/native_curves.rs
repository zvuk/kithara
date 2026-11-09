use std::num::{NonZeroU64, NonZeroUsize};

use kithara_platform::sync::Arc;
use kithara_signal::ExponentialApproach;
use kithara_sync::SpeedCurve;
use kithara_test_fixtures::signal::{Wave, deinterleave_left, goertzel_magnitude, mean_abs};
use num_traits::ToPrimitive;

use super::*;
use crate::{
    Warp,
    test_pools::{pools, sample_buffer},
};

const SAMPLE_RATE: u32 = 48_000;
const INITIAL_SPEED: f32 = 0.6;
const TARGET_SPEED: f32 = 1.6;
const CURVE_FRAMES: u64 = 96_000;
const CAPTURE_FRAMES: usize = 180_000;
const KICKS: usize = 8;
const KICK_LEAD: usize = 4_800;
const KICK_SPACING: usize = 24_000;
const ONSET_WINDOW: usize = 6_000;
const ENVELOPE_FRAMES: usize = 32;
const ONSET_TOLERANCE: f64 = 240.0;
const TONE_HZ: f64 = 440.0;
const PITCH_WINDOW: usize = 12_000;

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn native_curves_leaving_the_rate_limit_keep_requests_bounded(#[case] backend: StretchKind) {
    for curve in [
        SpeedCurve::Steps(Arc::from([(0, 4.0), (64, 4.0_f32.next_down())])),
        SpeedCurve::Ramp {
            to: 3.99,
            frames: NonZeroU64::new(96_000).expect("ramp duration"),
        },
        SpeedCurve::Ramp {
            to: 3.0,
            frames: NonZeroU64::new(96_000).expect("ramp duration"),
        },
    ] {
        for quantum in [64, 257, 1] {
            let spec = AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("sample rate"));
            let config = WarpConfig::builder()
                .backend(backend)
                .speed(4.0)
                .keylock(true)
                .build();
            let mut renderer = Warp::new((), &config).renderer(spec, pools());
            renderer.set_speed(curve.clone(), 1).expect("bounded curve");
            renderer
                .prepare_engine_latency(spec)
                .expect("native engine");
            let pcm = vec![0.25; 65_536 * 2];
            let mut source = 0;
            let mut rendered = 0;
            while rendered < 512 {
                renderer
                    .prepare_engine_latency(spec)
                    .expect("prepared next quantum");
                let meta = AudioChunkInfo {
                    spec,
                    frame_offset: source,
                    ..Default::default()
                };
                let frames = renderer
                    .prepare_source_window(meta, 65_536, quantum.min(512 - rendered))
                    .expect("bounded native window")
                    .get();
                let first = usize::try_from(source).expect("source frame") * 2;
                let output = renderer
                    .render_quantum(AudioChunk::new(
                        AudioChunkInfo {
                            frames: u32::try_from(frames).expect("source frames"),
                            ..meta
                        },
                        sample_buffer(&renderer.pools, &pcm[first..first + frames * 2]),
                    ))
                    .continue_value()
                    .expect("prepared shape")
                    .expect("valid curve emits PCM");
                assert!(output.frames() > 0);
                source += u64::try_from(frames).expect("admitted frames");
                rendered += output.frames();
            }
        }
    }
}
fn source_at(frame: f64, approaching: bool) -> f64 {
    let initial = f64::from(INITIAL_SPEED);
    let target = f64::from(TARGET_SPEED);
    let duration = CURVE_FRAMES.to_f64().expect("curve duration");
    if approaching {
        target * frame - (initial - target) * duration * (-frame / duration).exp_m1()
    } else {
        let ramp = frame.min(duration);
        initial * ramp
            + (target - initial) * ramp * ramp / (2.0 * duration)
            + target * (frame - duration).max(0.0)
    }
}

fn output_at(source: f64, approaching: bool) -> f64 {
    let mut lower = 0.0;
    let mut upper = CAPTURE_FRAMES.to_f64().expect("capture duration");
    assert!(source_at(upper, approaching) > source);
    for _ in 0..64 {
        let middle = (lower + upper) * 0.5;
        if source_at(middle, approaching) < source {
            lower = middle;
        } else {
            upper = middle;
        }
    }
    (lower + upper) * 0.5
}

fn kick_onset(samples: &[f32], expected: f64) -> f64 {
    let frame = expected.round().to_usize().expect("kick frame");
    let start = frame.saturating_sub(ONSET_WINDOW);
    let window = &samples[start..frame + ONSET_WINDOW];
    let mut envelope = window.windows(ENVELOPE_FRAMES).map(mean_abs);
    let peak = envelope.clone().fold(0.0_f32, f32::max);
    assert!(peak > 0.1, "the physical kick must survive rendering");
    let onset = envelope
        .position(|level| level >= peak * 0.5)
        .expect("kick onset");
    (start + onset).to_f64().expect("detected onset")
}

#[kithara::hang_watchdog]
fn render_curve(backend: StretchKind, approaching: bool, quantum: usize, pcm: &[f32]) -> Vec<f32> {
    let spec = AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("sample rate"));
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(INITIAL_SPEED)
        .keylock(true)
        .render_quantum_frames(NonZeroUsize::new(512).expect("prepared quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let curve = if approaching {
        SpeedCurve::Approach(
            ExponentialApproach::new(
                f64::from(INITIAL_SPEED),
                f64::from(TARGET_SPEED),
                CURVE_FRAMES.to_f64().expect("curve duration"),
            )
            .expect("tempo approach"),
        )
    } else {
        SpeedCurve::Ramp {
            to: TARGET_SPEED,
            frames: NonZeroU64::new(CURVE_FRAMES).expect("ramp duration"),
        }
    };
    renderer.set_speed(curve, 1).expect("moving tempo");
    renderer
        .prepare_engine_latency(spec)
        .expect("native engine");
    let capabilities = renderer
        .engine
        .as_ref()
        .expect("prepared engine")
        .capabilities();
    assert!(renderer.trajectory.requires_quanta());
    assert!(
        renderer
            .native_projection_rates(capabilities, true)
            .expect("native admission")
            .is_some(),
        "this moving law must exercise the native engine"
    );
    let mut input_frame = 0;
    let mut output_frame = 0;
    let mut capture = Vec::with_capacity(CAPTURE_FRAMES * 2);
    while output_frame < CAPTURE_FRAMES {
        hang_tick!();
        let budget = quantum.min(CAPTURE_FRAMES - output_frame);
        renderer
            .prepare_engine_latency(spec)
            .expect("continuous engine");
        let meta = AudioChunkInfo {
            spec,
            frame_offset: u64::try_from(input_frame).expect("input frame"),
            ..Default::default()
        };
        let frames = renderer
            .prepare_source_window(meta, pcm.len() / 2 - input_frame, budget)
            .expect("source window")
            .get();
        let output = renderer
            .render_quantum(AudioChunk::new(
                AudioChunkInfo {
                    frames: u32::try_from(frames).expect("source frames"),
                    ..meta
                },
                sample_buffer(
                    &renderer.pools,
                    &pcm[input_frame * 2..(input_frame + frames) * 2],
                ),
            ))
            .continue_value()
            .expect("prepared quantum")
            .expect("native output");
        assert!(output.frames() > 0 && output.frames() <= budget);
        assert!(
            renderer
                .projection
                .as_ref()
                .expect("native projection")
                .feed
                .is_some()
        );
        let span = output.meta.source_span.expect("source mapping");
        for offset in [0, output.frames()] {
            let (numerator, denominator) = span
                .source_ratio_at(u64::try_from(offset).expect("output offset"))
                .expect("mapped boundary");
            let actual = numerator.to_f64().expect("source numerator")
                / denominator.get().to_f64().expect("source denominator");
            let expected = source_at(
                (output_frame + offset).to_f64().expect("output boundary"),
                approaching,
            );
            assert!((actual - expected).abs() <= 1e-6, "{actual} vs {expected}");
        }
        assert!(output.samples.iter().all(|sample| sample.is_finite()));
        input_frame += frames;
        output_frame += output.frames();
        capture.extend_from_slice(&output.samples);
        hang_reset!();
    }
    capture
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn native_moving_keylock_keeps_physical_kicks_on_the_published_curve(#[case] backend: StretchKind) {
    let mut source = kithara_test_fixtures::unit_fixtures::stretch_kick_body();
    source.resize((CAPTURE_FRAMES * 2 + 65_536) * 2, 0.0);
    let tone = Wave::sine(TONE_HZ);
    for (frame, samples) in source.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        samples[1] = 0.25 * f32::from(tone.sample(frame, SAMPLE_RATE)) / f32::from(i16::MAX);
    }
    let kicks = deinterleave_left(&source, 2);
    let source_onsets: Vec<_> = (0..KICKS)
        .map(|index| {
            kick_onset(
                &kicks,
                (KICK_LEAD + index * KICK_SPACING)
                    .to_f64()
                    .expect("source kick"),
            )
        })
        .collect();
    for approaching in [false, true] {
        let whole = render_curve(backend, approaching, 512, &source);
        let split = render_curve(backend, approaching, 257, &source);
        assert_eq!(split, whole, "the output partition must not change PCM");
        let kicks = deinterleave_left(&whole, 2);
        for (index, &source) in source_onsets.iter().enumerate() {
            let expected = output_at(source, approaching);
            let actual = kick_onset(&kicks, expected);
            assert!(
                (actual - expected).abs() <= ONSET_TOLERANCE,
                "{backend:?}, approach={approaching}, kick {index}: {actual} vs {expected}"
            );
        }
        let tone: Vec<_> = whole
            .as_chunks::<2>()
            .0
            .iter()
            .map(|frame| frame[1])
            .collect();
        for first in [24_000, 72_000, 128_000] {
            let end = first + PITCH_WINDOW;
            let rate = (source_at(end.to_f64().expect("pitch end"), approaching)
                - source_at(first.to_f64().expect("pitch start"), approaching))
                / PITCH_WINDOW.to_f64().expect("pitch window");
            let samples = &tone[first..end];
            let held = goertzel_magnitude(samples, TONE_HZ, SAMPLE_RATE);
            let shifted = goertzel_magnitude(samples, TONE_HZ * rate, SAMPLE_RATE);
            assert!(
                held > shifted * 8.0,
                "{backend:?}, approach={approaching}, output {first}: held={held}, shifted={shifted}"
            );
        }
    }
}
