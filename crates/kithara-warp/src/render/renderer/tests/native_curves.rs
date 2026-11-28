use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{ExponentialApproach, SessionFrame};
use kithara_sync::{
    SessionBeat, Slew, SlewCause, SourceGrid, SpeedCurve, Tempo, TempoStep, TempoTrajectory,
    TrackBeat, TrackGrid, phase_error, slew,
};
use kithara_test_fixtures::signal::{Wave, deinterleave_left, goertzel_magnitude, mean_abs, rms};
use num_traits::ToPrimitive;

use super::*;
use crate::{
    Pitch, Warp, WarpControls,
    test_pools::{TestPools, pools, sample_buffer},
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
const FIRST_GESTURE_FRAME: usize = 24_001;

/// Renders `frames` output frames of a held level from source frame `source` on.
fn render_held(renderer: &mut WarpRenderer<TestPools>, source: &mut u64, frames: usize) {
    let spec = renderer.spec;
    let pcm = vec![0.25; 65_536 * 2];
    let mut rendered = 0;
    while rendered < frames {
        renderer
            .prepare_engine_latency(spec)
            .expect("native service");
        let meta = AudioChunkInfo {
            spec,
            frame_offset: *source,
            ..Default::default()
        };
        let window = renderer
            .prepare_source_window(meta, 65_536, (frames - rendered).min(480))
            .expect("prepared source")
            .get();
        let first = usize::try_from(*source).expect("source offset") * 2;
        let output = renderer
            .render_quantum(AudioChunk::new(
                AudioChunkInfo {
                    frames: u32::try_from(window).expect("source count"),
                    ..meta
                },
                sample_buffer(&renderer.pools, &pcm[first..first + window * 2]),
            ))
            .continue_value()
            .expect("prepared shape")
            .expect("warm native output");
        *source += u64::try_from(window).expect("source frames");
        rendered += output.frames();
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_cached(StretchKind::Signalsmith, 4_801, true)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_empty(StretchKind::Signalsmith, 4_800, false)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_cached(StretchKind::Bungee, 4_801, true)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_empty(StretchKind::Bungee, 4_800, false)
)]
fn native_retarget_rejects_incompatible_cached_source_geometry(
    #[case] backend: StretchKind,
    #[case] frames_to_render: usize,
    #[case] continuous: bool,
) {
    let spec = AudioSpec::new(2, NonZeroU32::new(8_000).expect("sample rate"));
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(0.6)
        .controls(WarpControls::builder().pitch(Pitch::HELD).build())
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 0;
    render_held(&mut renderer, &mut source, frames_to_render);
    let projection = renderer.projection.as_ref().expect("native cache");
    assert_eq!(projection.offset < projection.buffers[0].len(), continuous);
    let curve = if continuous {
        SpeedCurve::Approach(
            ExponentialApproach::new(f64::from(0.6_f32), 1.9, 1.0)
                .expect("continuous rapid approach"),
        )
    } else {
        SpeedCurve::Constant(1.05)
    };
    let mut replacement = renderer.trajectory.clone();
    replacement.replace(curve.clone()).expect("new law");
    assert!(
        !renderer
            .can_continue_native(&replacement)
            .expect("cached geometry admission"),
        "queued PCM from the old law cannot carry the new source geometry"
    );
    renderer.set_speed(curve, 1).expect("accepted retarget");
    assert!(
        renderer.transition_pending(),
        "incompatible queued output must retire"
    );
}

/// A speed step the renderer takes at least one engine latency ahead stays in
/// the engine: on its frame the new law replaces a law already at its speed.
/// A nearer step, which the engine has been fed past, is left to re-prime the
/// engine on its frame; a nearer step of another law moves the one taken, and
/// a reset withdraws it.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn native_speed_step_taken_a_latency_ahead_keeps_the_engine(#[case] backend: StretchKind) {
    let spec = AudioSpec::new(2, NonZeroU32::new(8_000).expect("sample rate"));
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(0.8)
        .controls(WarpControls::builder().pitch(Pitch::HELD).build())
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 0;
    render_held(&mut renderer, &mut source, 4_800);
    let latency = renderer.engine_latency().get();
    let lead = u64::try_from(latency).expect("engine latency");
    let far = SpeedCurve::Constant(1.25);
    let near = SpeedCurve::Constant(0.9);

    assert!(
        !renderer
            .anticipate_speed(&near, lead - 1)
            .expect("fed past"),
        "the engine has been fed past a step nearer than its latency"
    );
    assert!(
        renderer
            .anticipate_speed(&far, lead + 480)
            .expect("far step"),
        "a step at least one latency ahead enters the law"
    );
    assert!(
        !renderer
            .anticipate_speed(&far, lead + 480)
            .expect("held step"),
        "the law already holds the step"
    );
    assert!(
        renderer.anticipate_speed(&near, lead).expect("near step"),
        "a nearer step of another law moves the one taken"
    );
    assert_eq!(renderer.trajectory.frames_to_step(), Some(lead));
    render_held(&mut renderer, &mut source, latency);
    renderer.set_speed(near, 1).expect("the step's law");
    assert!(
        !renderer.transition_pending(),
        "the law on the step's frame continues the live engine"
    );

    render_held(&mut renderer, &mut source, 480);
    assert!(renderer.anticipate_speed(&far, lead).expect("next step"));
    renderer.reset();
    assert_eq!(renderer.trajectory.frames_to_step(), None);
    assert_eq!(
        renderer.trajectory.speed().expect("held speed").to_bits(),
        0.9_f32.to_bits(),
        "the law returns to the speed it held"
    );

    render_held(&mut renderer, &mut source, 4_800);
    renderer.set_speed(far, 2).expect("unannounced step");
    assert!(
        renderer.transition_pending(),
        "a step the engine never read ahead re-primes it"
    );
}

/// A Bungee packet can cross a step of its law, and its source mapping then
/// covers the frames from that step on only. While output before the step is
/// still queued, a law that differs there cannot be shown to match it.
#[cfg(feature = "stretch-bungee")]
#[kithara::test]
#[case::bungee(StretchKind::Bungee)]
fn queued_output_before_a_packet_step_rejects_another_law(#[case] backend: StretchKind) {
    let spec = AudioSpec::new(2, NonZeroU32::new(8_000).expect("sample rate"));
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(0.8)
        .controls(WarpControls::builder().pitch(Pitch::HELD).build())
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let mut source = 0;
    render_held(&mut renderer, &mut source, 4_800);
    let lead = u64::try_from(renderer.engine_latency().get()).expect("engine latency") + 480;
    assert!(
        renderer
            .anticipate_speed(&SpeedCurve::Constant(1.25), lead)
            .expect("far step")
    );
    let queued_before_step = |renderer: &WarpRenderer<TestPools>| {
        renderer.projection.as_ref().is_some_and(|projection| {
            projection.cached_span.is_some_and(|(from, _)| {
                from > 0 && projection.offset < from * usize::from(spec.channels)
            })
        })
    };
    let mut rendered = 0;
    while !queued_before_step(&renderer) {
        assert!(rendered < 8_000, "no packet crossed the step");
        render_held(&mut renderer, &mut source, 1);
        rendered += 1;
    }
    let step = renderer.trajectory.frames_to_step().expect("taken step");
    let mut earlier = renderer.trajectory.clone();
    earlier
        .replace(SpeedCurve::Steps(Arc::from([(step - 1, 1.25)])))
        .expect("earlier step");
    assert!(
        !renderer
            .can_continue_native(&earlier)
            .expect("cached geometry admission"),
        "output queued before the packet's step cannot carry another law"
    );
}

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
                .controls(WarpControls::builder().pitch(Pitch::HELD).build())
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

fn render_curve(backend: StretchKind, approaching: bool, quantum: usize, pcm: &[f32]) -> Vec<f32> {
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(INITIAL_SPEED)
        .controls(WarpControls::builder().pitch(Pitch::HELD).build())
        .render_quantum_frames(NonZeroUsize::new(512).expect("prepared quantum"))
        .build();
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
    render_controls(&config, [(0, 1, curve)], quantum, pcm, |frame| {
        source_at(frame, approaching)
    })
}

#[kithara::hang_watchdog]
fn render_controls(
    config: &WarpConfig,
    controls: impl IntoIterator<Item = (usize, u64, SpeedCurve)>,
    quantum: usize,
    pcm: &[f32],
    source_at: impl Fn(f64) -> f64,
) -> Vec<f32> {
    let spec = AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("sample rate"));
    let mut renderer = Warp::new((), config).renderer(spec, pools());
    let mut controls = controls.into_iter().peekable();
    let mut input_frame = 0;
    let mut output_frame = 0;
    let mut capture = Vec::with_capacity(CAPTURE_FRAMES * 2);
    while output_frame < CAPTURE_FRAMES {
        hang_tick!();
        while let Some((_, revision, curve)) =
            controls.next_if(|(frame, _, _)| *frame == output_frame)
        {
            if output_frame == FIRST_GESTURE_FRAME {
                let projection = renderer
                    .projection
                    .as_ref()
                    .expect("native output is prepared");
                assert!(
                    projection.offset < projection.buffers[0].len(),
                    "the first gesture interrupts a cached native packet"
                );
            }
            renderer.set_speed(curve, revision).expect("accepted tempo");
            assert!(renderer.trajectory.requires_quanta());
        }
        renderer
            .prepare_engine_latency(spec)
            .expect("continuous engine");
        if let Some((frame, _, curve)) = controls.peek() {
            renderer
                .anticipate_speed(
                    curve,
                    u64::try_from(*frame - output_frame).expect("future control lead"),
                )
                .expect("next native control is observable");
        }
        let capabilities = renderer
            .engine
            .as_ref()
            .expect("prepared engine")
            .capabilities();
        assert!(
            renderer
                .native_projection_rates(capabilities, true)
                .expect("native admission")
                .is_some(),
            "this law must exercise the native engine"
        );
        let due = controls
            .peek()
            .map_or(quantum, |(frame, _, _)| quantum.min(*frame - output_frame));
        let budget = due.min(CAPTURE_FRAMES - output_frame);
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
            let expected = source_at((output_frame + offset).to_f64().expect("output boundary"));
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

fn physical_source() -> (Vec<f32>, Vec<f64>) {
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
    (source, source_onsets)
}

#[kithara::test(timeout(Quick))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn native_moving_keylock_keeps_physical_kicks_on_the_published_curve(#[case] backend: StretchKind) {
    let (source, source_onsets) = physical_source();
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

struct GestureStep {
    frame: usize,
    source: f64,
    initial: f64,
    target: f64,
}

impl GestureStep {
    const TIME_CONSTANT: f64 = 2_400.0;

    fn source_at(&self, frame: f64) -> f64 {
        let elapsed = frame - self.frame.to_f64().expect("gesture frame");
        self.source + self.target * elapsed
            - (self.initial - self.target)
                * Self::TIME_CONSTANT
                * (-elapsed / Self::TIME_CONSTANT).exp_m1()
    }

    fn rate_at(&self, frame: f64) -> f64 {
        let elapsed = frame - self.frame.to_f64().expect("gesture frame");
        self.target + (self.initial - self.target) * (-elapsed / Self::TIME_CONSTANT).exp()
    }
}

fn gesture_steps(high: f64) -> Vec<GestureStep> {
    let initial = f64::from(1.05_f32);
    let mut steps = vec![GestureStep {
        frame: 0,
        source: 0.0,
        initial,
        target: initial,
    }];
    for index in 0..100 {
        // Leave native output queued when the first tempo command arrives.
        let frame = FIRST_GESTURE_FRAME + index * 960;
        let at = frame.to_f64().expect("gesture frame");
        let previous = steps.last().expect("initial tempo");
        steps.push(GestureStep {
            frame,
            source: previous.source_at(at),
            initial: previous.rate_at(at),
            target: if index % 2 == 0 { high } else { 1.05 },
        });
    }
    steps
}

fn gesture_source_at(steps: &[GestureStep], frame: f64) -> f64 {
    let index = steps.partition_point(|step| step.frame.to_f64().expect("gesture frame") <= frame);
    steps[index - 1].source_at(frame)
}

fn gesture_output_at(source: f64, source_at: impl Fn(f64) -> f64) -> f64 {
    let mut lower = 0.0;
    let mut upper = CAPTURE_FRAMES.to_f64().expect("capture duration");
    assert!(source_at(upper) > source);
    for _ in 0..64 {
        let middle = (lower + upper) * 0.5;
        if source_at(middle) < source {
            lower = middle;
        } else {
            upper = middle;
        }
    }
    (lower + upper) * 0.5
}

fn render_gesture(
    backend: StretchKind,
    steps: &[GestureStep],
    quantum: usize,
    pcm: &[f32],
) -> Vec<f32> {
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(1.05)
        .controls(WarpControls::builder().pitch(Pitch::HELD).build())
        .render_quantum_frames(NonZeroUsize::new(512).expect("prepared quantum"))
        .build();
    let controls = steps.iter().enumerate().skip(1).map(|(revision, step)| {
        let curve = ExponentialApproach::new(step.initial, step.target, GestureStep::TIME_CONSTANT)
            .expect("continuous accepted tempo");
        (
            step.frame,
            u64::try_from(revision).expect("revision"),
            SpeedCurve::Approach(curve),
        )
    });
    render_controls(&config, controls, quantum, pcm, |frame| {
        gesture_source_at(steps, frame)
    })
}

#[kithara::test(timeout(Long))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn native_tempo_gestures_keep_physical_kicks_and_pitch_on_the_accepted_curve(
    #[case] backend: StretchKind,
) {
    let (source, source_onsets) = physical_source();
    for high in [1.10, 1.90] {
        let steps = gesture_steps(high);
        let whole = render_gesture(backend, &steps, 512, &source);
        let split = render_gesture(backend, &steps, 257, &source);
        assert_eq!(
            split, whole,
            "{backend:?}, high={high}: gesture partitions must preserve PCM"
        );
        let kicks = deinterleave_left(&whole, 2);
        for (index, &source) in source_onsets.iter().enumerate() {
            let expected = gesture_output_at(source, |frame| gesture_source_at(&steps, frame));
            let actual = kick_onset(&kicks, expected);
            assert!(
                (actual - expected).abs() <= ONSET_TOLERANCE,
                "{backend:?}, high={high}, kick {index}: {actual} vs {expected}"
            );
        }
        let tone: Vec<_> = whole
            .as_chunks::<2>()
            .0
            .iter()
            .map(|frame| frame[1])
            .collect();
        for first in [48_000, 96_000, 144_000] {
            let end = first + PITCH_WINDOW;
            let rate = (gesture_source_at(&steps, end.to_f64().expect("pitch end"))
                - gesture_source_at(&steps, first.to_f64().expect("pitch start")))
                / PITCH_WINDOW.to_f64().expect("pitch window");
            let samples = &tone[first..end];
            let held = goertzel_magnitude(samples, TONE_HZ, SAMPLE_RATE);
            let shifted = goertzel_magnitude(samples, TONE_HZ * rate, SAMPLE_RATE);
            assert!(
                held > shifted * 8.0,
                "{backend:?}, high={high}, output {first}: held={held}, shifted={shifted}"
            );
        }
    }
}

fn accepted_area(laws: &[(u64, ExponentialApproach)], end: f64) -> f64 {
    laws.iter()
        .enumerate()
        .filter_map(|(index, (at, law))| {
            let start = at.to_f64().expect("law boundary");
            let after = laws
                .get(index + 1)
                .map_or(end, |(next, _)| next.to_f64().expect("next law boundary"));
            let through = end.min(after);
            (through > start).then(|| {
                law.advance_over(0.0, through - start)
                    .expect("accepted analytic area")
            })
        })
        .sum()
}

struct CommandedGesture {
    controls: Vec<(usize, u64, SpeedCurve)>,
    clock: Vec<f64>,
}

fn commanded_gesture(high: f64, initial: f32) -> CommandedGesture {
    const COMMANDS: usize = 100;
    const INTERVAL: usize = 960;
    let rate = NonZeroU32::new(SAMPLE_RATE).expect("source rate");
    let model = TrackGrid::new(
        (0..=32)
            .map(|ordinal| TrackBeat {
                at: 0.125 + f64::from(ordinal) * 0.5,
                ordinal: i64::from(ordinal),
            })
            .collect(),
        Vec::new(),
        None,
        120.0,
        Some(16.125),
    )
    .expect("source grid stays a quarter beat behind its declared source time");
    let source = SourceGrid::new(&model, rate, None, false).expect("covered source geometry");
    let mut host = TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: Tempo::new(126.0).expect("reference tempo"),
        },
        NonZeroU16::new(4).expect("meter"),
        rate,
    );
    let initial = f64::from(initial);
    let mut laws = vec![(
        0,
        ExponentialApproach::new(initial, initial, 0.0).expect("initial native rate"),
    )];
    let mut controls = Vec::with_capacity(COMMANDS);
    let mut clock = Vec::with_capacity(CAPTURE_FRAMES + 1);
    let mut origin_frame = 0;
    let mut origin_source = 0.0;
    clock.push(0.0);
    for frame in 0..CAPTURE_FRAMES {
        if frame >= FIRST_GESTURE_FRAME
            && (frame - FIRST_GESTURE_FRAME).is_multiple_of(INTERVAL)
            && controls.len() < COMMANDS
        {
            let elapsed = (frame - origin_frame)
                .to_f64()
                .expect("elapsed accepted frames");
            origin_source += accepted_area(&laws, elapsed);
            let (start, law) = laws
                .iter()
                .rev()
                .find(|(at, _)| at.to_f64().expect("law boundary") <= elapsed)
                .expect("a current accepted law");
            let from = law
                .rate_at(elapsed - start.to_f64().expect("current law boundary"))
                .expect("actual accepted velocity");
            let at = SessionFrame::new(i64::try_from(frame).expect("command session frame"));
            let target = if controls.len() % 2 == 0 { high } else { 126.0 };
            host.push(at, Tempo::new(target).expect("target tempo"))
                .expect("real Host tempo smoothing");
            let cue = Duration::from_secs_f64(origin_source / f64::from(SAMPLE_RATE));
            let phase = phase_error(&host, &source, cue, at).expect("covered source phase");
            assert!(
                phase.beats.min(phase.period - phase.beats) > 0.1,
                "the gesture exercises an active source phase correction"
            );
            let Some(Slew::Corrects { curve, .. }) = slew(
                &host,
                &source,
                cue,
                at,
                0.001,
                SlewCause::Commanded { from },
                4 * u64::from(SAMPLE_RATE),
            ) else {
                panic!("the Host/source pair admits the tempo programme");
            };
            laws = match &curve {
                SpeedCurve::Constant(speed) => vec![(
                    0,
                    ExponentialApproach::new(f64::from(*speed), f64::from(*speed), 0.0)
                        .expect("constant accepted law"),
                )],
                SpeedCurve::Approach(law) => vec![(0, *law)],
                SpeedCurve::ApproachSteps(parts) => parts.to_vec(),
                other => panic!("unexpected source programme: {other:?}"),
            };
            let revision = u64::try_from(controls.len() + 1).expect("control revision");
            controls.push((frame, revision, curve));
            origin_frame = frame;
        }
        clock.push(
            origin_source
                + accepted_area(
                    &laws,
                    (frame + 1 - origin_frame)
                        .to_f64()
                        .expect("next accepted frame"),
                ),
        );
    }
    assert_eq!(controls.len(), COMMANDS);
    assert!(clock.windows(2).all(|pair| pair[1] > pair[0]));
    CommandedGesture { controls, clock }
}

fn sampled_source_at(clock: &[f64], frame: f64) -> f64 {
    let first = frame.floor().to_usize().expect("bounded output frame");
    let source = clock[first];
    if first + 1 == clock.len() {
        assert_eq!(frame, first.to_f64().expect("capture endpoint"));
        source
    } else {
        source + (clock[first + 1] - source) * (frame - first.to_f64().expect("sample frame"))
    }
}

#[kithara::test]
#[cfg_attr(feature = "stretch-signalsmith", case::signalsmith_behind(StretchKind::Signalsmith, -0.0005))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_ahead(StretchKind::Signalsmith, 0.0005)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee_behind(StretchKind::Bungee, -0.0005))]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_ahead(StretchKind::Bungee, 0.0005)
)]
fn commanded_slew_gestures_keep_physical_phase_pitch_and_level(
    #[case] backend: StretchKind,
    #[case] nudge: f32,
) {
    let (source, source_onsets) = physical_source();
    let initial = 1.05_f32 + nudge;
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(initial)
        .controls(WarpControls::builder().pitch(Pitch::HELD).build())
        .render_quantum_frames(NonZeroUsize::new(512).expect("prepared quantum"))
        .build();
    let high = 132.0;
    let CommandedGesture { controls, clock } = commanded_gesture(high, initial);
    let whole = render_controls(&config, controls.clone(), 512, &source, |frame| {
        sampled_source_at(&clock, frame)
    });
    let split = render_controls(&config, controls, 257, &source, |frame| {
        sampled_source_at(&clock, frame)
    });
    assert_eq!(
        split, whole,
        "{backend:?}, high={high}, nudge={nudge}: accepted programmes are partition independent"
    );
    let kicks = deinterleave_left(&whole, 2);
    for (index, &onset) in source_onsets.iter().enumerate() {
        let expected = gesture_output_at(onset, |frame| sampled_source_at(&clock, frame));
        let actual = kick_onset(&kicks, expected);
        assert!(
            (actual - expected).abs() <= ONSET_TOLERANCE,
            "{backend:?}, high={high}, nudge={nudge}, kick {index}: {actual} vs {expected}"
        );
    }
    let tone: Vec<_> = whole
        .as_chunks::<2>()
        .0
        .iter()
        .map(|frame| frame[1])
        .collect();
    let window = usize::try_from(SAMPLE_RATE / 20).expect("50ms level window");
    let hop = usize::try_from(SAMPLE_RATE / 100).expect("10ms level hop");
    let reference = rms(&tone[FIRST_GESTURE_FRAME - window * 2..FIRST_GESTURE_FRAME]);
    assert!(reference > 0.1, "the steady keylocked tone is audible");
    let minimum = tone[FIRST_GESTURE_FRAME..FIRST_GESTURE_FRAME + 100 * 960]
        .windows(window)
        .step_by(hop)
        .map(rms)
        .fold(f32::INFINITY, f32::min);
    let minimum_db = 20.0 * (minimum / reference).log10();
    assert!(
        minimum_db >= -1.0,
        "{backend:?}, high={high}, nudge={nudge}: accepted programme loses {minimum_db:.3}dB"
    );
    for first in [48_000, 96_000, 144_000] {
        let end = first + PITCH_WINDOW;
        let rate = (clock[end] - clock[first]) / PITCH_WINDOW.to_f64().expect("pitch window");
        let held = goertzel_magnitude(&tone[first..end], TONE_HZ, SAMPLE_RATE);
        let shifted = goertzel_magnitude(&tone[first..end], TONE_HZ * rate, SAMPLE_RATE);
        assert!(
            held > shifted * 8.0,
            "{backend:?}, high={high}, nudge={nudge}, frame={first}: held={held}, shifted={shifted}"
        );
    }
}
