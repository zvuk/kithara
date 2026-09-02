//! Conformance suite for the exact-span elastic contract.
//!
//! Every compiled-in engine runs the same conformance cases, including the
//! mandatory priming lifecycle.
//! Every observable lifecycle and audio behavior is shared; backend-specific
//! tests cover only private preparation and storage mechanics.
use std::{f32::consts::TAU, ops::RangeInclusive};

use kithara_bufpool::testing::{pools as default_pools, pools_with_budget as pools};
use kithara_stretch::{
    ElasticCapabilities, ElasticConfig, ElasticEngine, ElasticError, ElasticRequest,
    ElasticSpanConfig, StretchKind, build_engine,
};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

const CHANNELS: usize = 2;
const CONTROL_QUANTUM: usize = 64;
const LANDMARK_FREQUENCIES: [f64; 12] = [
    1_125.0, 1_875.0, 2_625.0, 3_375.0, 4_125.0, 4_875.0, 5_625.0, 6_375.0, 7_125.0, 7_875.0,
    8_625.0, 9_375.0,
];
const SAMPLE_RATE: u32 = 48_000;
const SHORT_MARKER_HZ: f64 = 15_000.0;
const SHORT_MARKER_WINDOW_FRAMES: usize = 16;
const TONE_HZ: f64 = 440.0;
const TERMINAL_HIGH_HZ: f64 = 6_000.0;
const TERMINAL_LOW_HZ: f64 = 1_500.0;
const TERMINAL_WINDOW_FRAMES: usize = 64;

fn prepared_backend(
    backend: StretchKind,
    max_source_frames: usize,
    max_output_frames: usize,
) -> Box<dyn ElasticEngine> {
    let config = ElasticConfig::builder()
        .backend(backend)
        .pools(default_pools())
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .max_source_frames(max_source_frames)
        .max_output_frames(max_output_frames)
        .build()
        .expect("the test configuration is valid");
    build_engine(config).expect("the selected engine prepares for a valid shape")
}

fn prepared_backend_with_rate_envelope(
    backend: StretchKind,
    max_source_frames: usize,
    max_output_frames: usize,
    rate_envelope: RangeInclusive<f64>,
) -> Box<dyn ElasticEngine> {
    let config = ElasticConfig::builder()
        .backend(backend)
        .pools(default_pools())
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .max_source_frames(max_source_frames)
        .max_output_frames(max_output_frames)
        .rate_envelope(rate_envelope)
        .build()
        .expect("the test configuration is valid");
    build_engine(config).expect("the selected engine prepares for a valid shape")
}

fn interleaved_signal(frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let sample = if frame % 64 < 32 { 0.25 } else { -0.25 };
            [sample, -sample]
        })
        .collect()
}

fn drain_terminal(engine: &mut dyn ElasticEngine) -> Vec<f32> {
    const MAX_CHUNKS: usize = 256;

    let mut chunk = vec![0.0; CONTROL_QUANTUM * CHANNELS];
    let mut drained = Vec::new();
    for _ in 0..MAX_CHUNKS {
        chunk.fill(0.0);
        let step = engine.flush(&mut chunk).expect("terminal flush");
        let frames = step.frames();
        assert!(frames > 0, "an active drain step carries real audio frames");
        drained.extend_from_slice(&chunk[..frames * CHANNELS]);
        if step.complete() {
            let completed = engine.flush(&mut chunk).expect("completed drain");
            assert_eq!(completed.frames(), 0);
            assert!(completed.complete());
            return drained;
        }
    }
    panic!("terminal drain must converge to an empty flush");
}

fn impulse_markers(frames: usize, offset: usize) -> Vec<f32> {
    marker_signal(frames, offset, |index| {
        if index.is_multiple_of(64) {
            let marker_index = u16::try_from((index / 64) % 7)
                .expect("invariant: marker index is bounded below 7");
            0.5 + f32::from(marker_index) / 14.0
        } else {
            0.0
        }
    })
}

fn marker_signal(
    frames: usize,
    offset: usize,
    mut marker_at: impl FnMut(usize) -> f32,
) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let index = offset.wrapping_add(frame);
            let marker = marker_at(index);
            [marker, marker * -0.5]
        })
        .collect()
}

fn continuous_tone(frames: usize, offset: usize) -> Vec<f32> {
    tone_signal(frames, offset, TONE_HZ)
}

fn tone_signal(frames: usize, offset: usize, frequency: f64) -> Vec<f32> {
    let phase_step = TAU
        * frequency
            .to_f32()
            .expect("the fixture frequency fits in f32")
        / SAMPLE_RATE
            .to_f32()
            .expect("the fixture sample rate fits in f32");
    marker_signal(frames, offset, |index| {
        (index.to_f32().expect("the fixture timeline fits in f32") * phase_step).sin() * 0.5
    })
}

fn landmark_signal(frames: usize, landmarks: &[usize]) -> Vec<f32> {
    assert!(!landmarks.is_empty());
    assert!(frames >= landmarks.len());
    (0..frames)
        .flat_map(|frame| {
            let slot = frame * landmarks.len() / frames;
            let slot_start = slot * frames / landmarks.len();
            let slot_end = (slot + 1) * frames / landmarks.len();
            let slot_frames = slot_end - slot_start;
            let slot_position = frame - slot_start;
            let guarded = slot_position < slot_frames / 8
                || slot_position >= slot_frames.saturating_mul(7) / 8;
            let frequency = if guarded {
                TONE_HZ
            } else {
                LANDMARK_FREQUENCIES[landmarks[slot]]
            }
            .to_f32()
            .expect("the landmark frequency fits in f32");
            let position = slot_position
                .to_f32()
                .expect("the landmark position fits in f32");
            let sample_rate = SAMPLE_RATE
                .to_f32()
                .expect("the fixture sample rate fits in f32");
            let sample = (TAU * frequency * position / sample_rate).sin() * 0.5;
            [sample, sample * -0.5]
        })
        .collect()
}

fn tone_window_magnitude(
    samples: &[f32],
    start: usize,
    window_frames: usize,
    frequency: f64,
) -> f64 {
    let phase_step = std::f64::consts::TAU * frequency / f64::from(SAMPLE_RATE);
    let (real, imaginary) = (0..window_frames).fold((0.0, 0.0), |(real, imaginary), offset| {
        let sample = f64::from(samples[(start + offset) * CHANNELS]);
        let phase = phase_step
            * offset
                .to_f64()
                .expect("the marker window offset fits in f64");
        (
            real + sample * phase.cos(),
            imaginary - sample * phase.sin(),
        )
    });
    real.hypot(imaginary)
        / window_frames
            .to_f64()
            .expect("the marker window length fits in f64")
}

fn first_audible_frame(samples: &[f32], channels: usize) -> Option<usize> {
    samples
        .chunks_exact(channels)
        .position(|frame| frame.iter().any(|sample| sample.abs() >= 1.0e-4))
}

fn terminal_marker_signal_with_span(frames: usize, marker_span: usize) -> Vec<f32> {
    let marker_frames = frames.min(marker_span);
    let marker_start = frames - marker_frames;
    (0..frames)
        .flat_map(|frame| {
            let sample = if frame < marker_start {
                0.0
            } else {
                let marker_frame = frame - marker_start;
                let frequency = if marker_frame < marker_frames / 2 {
                    TERMINAL_LOW_HZ
                } else {
                    TERMINAL_HIGH_HZ
                };
                let frequency = frequency
                    .to_f32()
                    .expect("the terminal marker frequency fits in f32");
                let marker_frame = marker_frame
                    .to_f32()
                    .expect("the terminal marker position fits in f32");
                let sample_rate = SAMPLE_RATE
                    .to_f32()
                    .expect("the terminal sample rate fits in f32");
                (TAU * frequency * marker_frame / sample_rate).sin() * 0.5
            };
            [sample, sample * -0.5]
        })
        .collect()
}

fn short_marker_signal(frames: usize) -> Vec<f32> {
    tone_signal(frames, 0, SHORT_MARKER_HZ)
}

fn short_marker_is_present(samples: &[f32]) -> bool {
    const MINIMUM_MAGNITUDE: f64 = 0.05;
    const STEP_FRAMES: usize = 4;

    let frames = samples.len() / CHANNELS;
    frames >= SHORT_MARKER_WINDOW_FRAMES
        && (0..=frames - SHORT_MARKER_WINDOW_FRAMES)
            .step_by(STEP_FRAMES)
            .any(|start| {
                tone_window_magnitude(samples, start, SHORT_MARKER_WINDOW_FRAMES, SHORT_MARKER_HZ)
                    >= MINIMUM_MAGNITUDE
            })
}

fn strongest_tone_window(samples: &[f32], frequency: f64) -> Option<(usize, f64)> {
    let frames = samples.len() / CHANNELS;
    (frames >= TERMINAL_WINDOW_FRAMES).then(|| {
        (0..=frames - TERMINAL_WINDOW_FRAMES)
            .step_by(CONTROL_QUANTUM)
            .map(|start| {
                (
                    start,
                    tone_window_magnitude(samples, start, TERMINAL_WINDOW_FRAMES, frequency),
                )
            })
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .expect("a terminal analysis window exists")
    })
}

fn dominant_landmark_sequence(samples: &[f32], landmarks: &[usize]) -> Vec<usize> {
    const DOMINANCE: f64 = 1.25;
    const MINIMUM_MAGNITUDE: f64 = 0.03;
    const MINIMUM_RUN_WINDOWS: usize = 3;
    const WINDOW_FRAMES: usize = 128;

    let frames = samples.len() / CHANNELS;
    if frames < WINDOW_FRAMES {
        return Vec::new();
    }
    let mut sequence = Vec::new();
    let mut run_label = None;
    let mut run_windows = 0usize;
    for start in (0..=frames - WINDOW_FRAMES).step_by(CONTROL_QUANTUM) {
        let mut best = (usize::MAX, 0.0);
        let mut second = 0.0;
        let guard = tone_window_magnitude(samples, start, WINDOW_FRAMES, TONE_HZ);
        for landmark in landmarks.iter().copied() {
            let magnitude = tone_window_magnitude(
                samples,
                start,
                WINDOW_FRAMES,
                LANDMARK_FREQUENCIES[landmark],
            );
            if magnitude > best.1 {
                second = best.1;
                best = (landmark, magnitude);
            } else if magnitude > second {
                second = magnitude;
            }
        }
        let label = (best.1 >= MINIMUM_MAGNITUDE
            && best.1 >= second * DOMINANCE
            && best.1 >= guard * DOMINANCE)
            .then_some(best.0);
        if label == run_label {
            run_windows += 1;
            continue;
        }
        if run_windows >= MINIMUM_RUN_WINDOWS
            && let Some(label) = run_label
            && sequence.last() != Some(&label)
        {
            sequence.push(label);
        }
        run_label = label;
        run_windows = 1;
    }
    if run_windows >= MINIMUM_RUN_WINDOWS
        && let Some(label) = run_label
        && sequence.last() != Some(&label)
    {
        sequence.push(label);
    }
    sequence
}

fn landmarks_appear_once_in_order(samples: &[f32], landmarks: &[usize]) -> bool {
    let sequence = dominant_landmark_sequence(samples, landmarks);
    sequence == landmarks
}

#[kithara::test]
fn indexed_landmark_oracle_rejects_reorder_omission_replay_and_partial_drop() {
    const MARKER_FRAMES: usize = 2_048;

    let fixture = |order: &[usize]| {
        let mut samples = Vec::new();
        for &landmark in order {
            samples.extend_from_slice(&landmark_signal(MARKER_FRAMES, &[landmark]));
        }
        samples
    };
    let expected = [0, 1, 2, 3, 4];
    let reordered = [0, 2, 1, 3, 4];
    let omitted = [0, 1, 3, 4];
    let replayed = [0, 1, 2, 1, 2, 3, 4];
    let mut partial = fixture(&expected);
    partial.truncate(
        (MARKER_FRAMES * (expected.len() - 1) + MARKER_FRAMES / 8 + CONTROL_QUANTUM) * CHANNELS,
    );

    assert!(landmarks_appear_once_in_order(
        &fixture(&expected),
        &expected
    ));
    assert!(!landmarks_appear_once_in_order(
        &fixture(&reordered),
        &expected,
    ));
    assert!(!landmarks_appear_once_in_order(
        &fixture(&omitted),
        &expected,
    ));
    assert!(!landmarks_appear_once_in_order(
        &fixture(&replayed),
        &expected,
    ));
    assert!(!landmarks_appear_once_in_order(&partial, &expected));

    let short = short_marker_signal(CONTROL_QUANTUM);
    assert!(short_marker_is_present(&short));
    assert!(!short_marker_is_present(&vec![0.0; short.len()]));
    assert!(!short_marker_is_present(&continuous_tone(
        CONTROL_QUANTUM,
        0
    )));
}

fn terminal_markers_are_ordered(samples: &[f32]) -> bool {
    const MINIMUM_MAGNITUDE: f64 = 0.05;

    let Some((low_position, low_magnitude)) = strongest_tone_window(samples, TERMINAL_LOW_HZ)
    else {
        return false;
    };
    let Some((high_position, high_magnitude)) = strongest_tone_window(samples, TERMINAL_HIGH_HZ)
    else {
        return false;
    };
    low_magnitude >= MINIMUM_MAGNITUDE
        && high_magnitude >= MINIMUM_MAGNITUDE
        && low_position + TERMINAL_WINDOW_FRAMES <= high_position
}

fn terminal_pattern_is_valid(samples: &[f32], expected_frames: usize) -> bool {
    samples.len() == expected_frames.saturating_mul(CHANNELS)
        && terminal_markers_are_ordered(samples)
}

fn assert_exact_samples(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual, expected, "sample {index} differs");
    }
}

/// The source span an engine accepts at a declared envelope edge: the planner
/// quantizes the same way, so a conformance request is never a rounding step
/// outside the window it is meant to exercise.
fn source_frames_at(rate: f64, output_frames: usize, round_up: bool) -> usize {
    let frames = output_frames
        .to_f64()
        .map(|frames| frames * rate)
        .expect("invariant: the test block fits in f64");
    let frames = if round_up {
        frames.ceil()
    } else {
        frames.floor()
    };
    frames
        .to_usize()
        .expect("invariant: the edge span fits in usize")
}

fn edge_request(
    capabilities: ElasticCapabilities,
    output_frames: usize,
    minimum: bool,
) -> ElasticRequest {
    let envelope = capabilities.rate_envelope();
    let rate = if minimum {
        envelope.min_source_frames_per_output()
    } else {
        envelope.max_source_frames_per_output()
    };
    ElasticRequest::new(
        source_frames_at(rate, output_frames, minimum),
        output_frames,
    )
    .expect("the envelope edge request is valid")
}

fn rate_aware_latency_frames(capabilities: ElasticCapabilities, request: ElasticRequest) -> usize {
    let source_frames = request
        .source_frames()
        .to_f64()
        .expect("the source span fits in f64");
    let output_frames = request
        .output_frames()
        .to_f64()
        .expect("the output span fits in f64");
    capabilities
        .latency()
        .source_frames()
        .to_f64()
        .map(|frames| (frames / (source_frames / output_frames)).ceil())
        .and_then(|frames| frames.to_usize())
        .and_then(|frames| frames.checked_add(capabilities.latency().output_frames()))
        .expect("the rate-aware latency fits in usize")
}

fn rate_aware_terminal_source_frames(
    capabilities: ElasticCapabilities,
    request: ElasticRequest,
) -> usize {
    let rate = request
        .source_frames()
        .to_f64()
        .zip(request.output_frames().to_f64())
        .map(|(source, output)| source / output)
        .expect("the request spans fit in f64");
    capabilities
        .latency()
        .source_frames()
        .checked_add(source_frames_at(
            rate,
            capabilities.latency().output_frames(),
            true,
        ))
        .expect("the terminal source span fits in usize")
}

fn edge_requests(capabilities: ElasticCapabilities) -> [ElasticRequest; 3] {
    let unity_frames = capabilities
        .max_source_frames()
        .min(capabilities.max_output_frames());
    let slow_output_frames = unity_frames - unity_frames % 20;
    [
        (slow_output_frames / 20, slow_output_frames),
        (unity_frames, unity_frames),
        (unity_frames, unity_frames / 4),
    ]
    .map(|(source_frames, output_frames)| {
        ElasticRequest::new(source_frames, output_frames)
            .expect("invariant: prepared-domain request is non-empty")
    })
}

mod facade {
    use super::*;

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn renders_the_requested_output_frame_count(#[case] backend: StretchKind) {
        let mut engine = prepared_backend(backend, 8192, 8192);
        let request = ElasticRequest::new(4800, 4000).expect("the request is non-empty");
        let source = interleaved_signal(request.source_frames());
        let mut output = vec![f32::NAN; request.output_frames() * CHANNELS];

        engine
            .process(request, &source, &mut output)
            .expect("the request is inside the prepared envelope");

        assert_eq!(output.len(), 4000 * CHANNELS);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn renders_exact_spans_at_both_declared_rate_edges(#[case] backend: StretchKind) {
        let mut engine = prepared_backend(backend, 8192, 4096);
        let capabilities = engine.capabilities();

        for request in edge_requests(capabilities) {
            let source = interleaved_signal(request.source_frames());
            let mut output = vec![f32::NAN; request.output_frames() * CHANNELS];

            engine
                .process(request, &source, &mut output)
                .expect("a declared edge rate is supported");

            assert_eq!(output.len(), request.output_frames() * CHANNELS);
            assert!(output.iter().all(|sample| sample.is_finite()));
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn output_is_independent_of_request_partitioning(#[case] backend: StretchKind) {
        for (source_frames, output_frames, source_partition, output_partition) in [
            (16_384, 16_384, 512, 512),
            (8192, 10_240, 512, 640),
            (16_384, 8192, 1024, 512),
        ] {
            let mut whole = prepared_backend(backend, source_frames, output_frames);
            let mut partitioned = prepared_backend(backend, source_frames, output_frames);
            let source = impulse_markers(source_frames, 0);
            let mut whole_output = vec![0.0; output_frames * CHANNELS];
            whole
                .process(
                    ElasticRequest::new(source_frames, output_frames).expect("whole request"),
                    &source,
                    &mut whole_output,
                )
                .expect("the whole block renders");

            let mut partitioned_output = vec![0.0; output_frames * CHANNELS];
            for (source, output) in source
                .chunks_exact(source_partition * CHANNELS)
                .zip(partitioned_output.chunks_exact_mut(output_partition * CHANNELS))
            {
                partitioned
                    .process(
                        ElasticRequest::new(source_partition, output_partition)
                            .expect("partition request"),
                        source,
                        output,
                    )
                    .expect("every partition renders");
            }

            assert!(
                first_audible_frame(&whole_output, CHANNELS).is_some(),
                "the block must outlast the engine latency for this to compare audio"
            );
            assert_exact_samples(&partitioned_output, &whole_output);
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn keeps_capabilities_stable_through_rate_changes(#[case] backend: StretchKind) {
        const FAST_OUTPUT_FRAMES: usize = 100;
        const FAST_SOURCE_FRAMES: usize = 400;
        const SLOW_OUTPUT_FRAMES: usize = 8000;
        const SLOW_SOURCE_FRAMES: usize = 400;

        let mut engine = prepared_backend(backend, 8192, 8192);
        let capabilities = engine.capabilities();
        let mut source_position = 0;
        let mut previous: Option<[f32; CHANNELS]> = None;

        for request in [
            ElasticRequest::new(8192, 8192).expect("unity request"),
            ElasticRequest::new(8192, 8192).expect("unity request"),
            ElasticRequest::new(4096, 4096).expect("unity request"),
            ElasticRequest::new(SLOW_SOURCE_FRAMES, SLOW_OUTPUT_FRAMES).expect("slowest request"),
            ElasticRequest::new(FAST_SOURCE_FRAMES, FAST_OUTPUT_FRAMES).expect("fastest request"),
            ElasticRequest::new(4096, 4096).expect("unity request"),
        ] {
            let source = continuous_tone(request.source_frames(), source_position);
            let mut output = vec![f32::NAN; request.output_frames() * CHANNELS];

            engine
                .process(request, &source, &mut output)
                .expect("the request is supported");

            assert!(output.iter().all(|sample| sample.is_finite()));
            assert_eq!(engine.capabilities(), capabilities);
            if let Some(previous) = previous {
                for channel in 0..CHANNELS {
                    assert!(
                        (output[channel] - previous[channel]).abs() <= 0.1,
                        "unprimed rate change must keep the output boundary continuous: backend={backend:?}, request={request:?}, channel={channel}"
                    );
                }
            }
            previous = Some([
                output[(request.output_frames() - 1) * CHANNELS],
                output[request.output_frames() * CHANNELS - 1],
            ]);
            source_position += request.source_frames();
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn pitch_control_is_independent_of_exact_frame_advance(#[case] backend: StretchKind) {
        let mut reference = prepared_backend(backend, 8192, 8192);
        let mut pitched = prepared_backend(backend, 8192, 8192);
        let request = ElasticRequest::new(4096, 4096).expect("unity request");
        let mut changed = false;

        pitched.set_pitch(1.25).expect("positive pitch scale");
        for block in 0..4 {
            let source = marker_signal(request.source_frames(), block * 4096, |index| {
                f32::from(u16::try_from(index % 997).expect("marker index fits")) / 997.0 - 0.5
            });
            let mut reference_output = vec![f32::NAN; request.output_frames() * CHANNELS];
            let mut pitched_output = vec![f32::NAN; request.output_frames() * CHANNELS];
            reference
                .process(request, &source, &mut reference_output)
                .expect("reference engine renders the exact span");
            pitched
                .process(request, &source, &mut pitched_output)
                .expect("pitch does not replace exact frame control");

            assert!(pitched_output.iter().all(|sample| sample.is_finite()));
            changed |= pitched_output != reference_output;
        }

        assert!(changed, "pitch control must alter rendered samples");
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn rejects_invalid_pitch_scales(#[case] backend: StretchKind) {
        const MIN_NATIVE_RANGE: f64 = 0.25;
        const MAX_NATIVE_RANGE: f64 = 4.0;
        const BELOW_NATIVE_RANGE: f64 = 0.249;
        const ABOVE_NATIVE_RANGE: f64 = 4.001;

        let mut engine = prepared_backend(backend, 8192, 8192);

        for scale in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            BELOW_NATIVE_RANGE,
            ABOVE_NATIVE_RANGE,
        ] {
            assert!(matches!(
                engine.set_pitch(scale),
                Err(ElasticError::InvalidPitch(_))
            ));
        }

        for scale in [MIN_NATIVE_RANGE, MAX_NATIVE_RANGE] {
            engine
                .set_pitch(scale)
                .expect("the common native pitch boundary is supported");
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn terminal_flush_reaches_last_source_audio_at_each_rate(#[case] backend: StretchKind) {
        const FRAMES: usize = 8192;
        for request in [
            ElasticRequest::new(FRAMES / 2, FRAMES).expect("half-speed request"),
            ElasticRequest::new(FRAMES, FRAMES).expect("unity request"),
            ElasticRequest::new(FRAMES, FRAMES / 2).expect("double-speed request"),
        ] {
            let mut engine = prepared_backend(backend, FRAMES, FRAMES);
            let capabilities = engine.capabilities();
            let marker_frames = rate_aware_terminal_source_frames(capabilities, request);
            let source = terminal_marker_signal_with_span(request.source_frames(), marker_frames);
            let mut output = vec![0.0; request.output_frames() * CHANNELS];
            engine
                .process(request, &source, &mut output)
                .expect("the source block renders");
            let terminal = drain_terminal(engine.as_mut());
            let drained = terminal.len() / CHANNELS;
            let expected_drained = rate_aware_latency_frames(capabilities, request);
            assert_eq!(
                drained, expected_drained,
                "terminal drain must preserve the rate-aware latency span: backend={backend:?}, request={request:?}"
            );
            assert!(
                terminal_pattern_is_valid(&terminal, expected_drained),
                "terminal drain must preserve both ordered source markers: backend={backend:?}, request={request:?}, drained={drained}"
            );

            assert!(
                !terminal_pattern_is_valid(
                    &terminal[..terminal.len() - CHANNELS],
                    expected_drained,
                ),
                "the oracle must reject a one-frame terminal truncation"
            );
            let mut padded = terminal.clone();
            padded.extend(std::iter::repeat_n(0.0, CHANNELS));
            assert!(
                !terminal_pattern_is_valid(&padded, expected_drained),
                "the oracle must reject one synthetic terminal frame"
            );
            let reversed = terminal
                .chunks_exact(CHANNELS)
                .rev()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            assert!(
                !terminal_pattern_is_valid(&reversed, expected_drained),
                "the oracle must reject a reversed terminal pattern"
            );
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_minimum(StretchKind::Signalsmith, 0.05)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_unity(StretchKind::Signalsmith, 1.0)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_maximum(StretchKind::Signalsmith, 4.0)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_minimum(StretchKind::Bungee, 0.05)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_unity(StretchKind::Bungee, 1.0)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_maximum(StretchKind::Bungee, 4.0)
    )]
    fn terminal_flush_drains_through_caller_sized_quantums(
        #[case] backend: StretchKind,
        #[case] rate: f64,
    ) {
        const OUTPUT_FRAMES: usize = 8_000;
        const MAX_SOURCE_FRAMES: usize = OUTPUT_FRAMES * 4;

        let mut engine = prepared_backend(backend, MAX_SOURCE_FRAMES, OUTPUT_FRAMES);
        let capabilities = engine.capabilities();
        let source_frames = source_frames_at(rate, OUTPUT_FRAMES, false);
        let request = ElasticRequest::new(source_frames, OUTPUT_FRAMES)
            .expect("the exact-rate request is non-empty");
        let marker_frames = rate_aware_terminal_source_frames(capabilities, request);
        let source = terminal_marker_signal_with_span(source_frames, marker_frames);
        let mut output = vec![0.0; OUTPUT_FRAMES * CHANNELS];
        engine
            .process(request, &source, &mut output)
            .expect("the source block renders");

        let latency = capabilities.latency();
        let expected_frames = latency
            .source_frames()
            .to_f64()
            .map(|frames| (frames / rate).ceil())
            .and_then(|frames| frames.to_usize())
            .and_then(|frames| frames.checked_add(latency.output_frames()))
            .expect("the terminal span fits in usize");
        let mut quantum = [0.0; CONTROL_QUANTUM * CHANNELS];
        let mut terminal = Vec::with_capacity(expected_frames * CHANNELS);
        let max_steps = expected_frames.div_ceil(CONTROL_QUANTUM);

        for _ in 0..max_steps {
            quantum.fill(0.0);
            let step = engine
                .flush(&mut quantum)
                .expect("terminal flush accepts caller-sized storage");
            assert!(step.frames() > 0, "an active drain step is non-empty");
            assert!(
                step.frames() <= CONTROL_QUANTUM,
                "a terminal drain step must fit the caller quantum: backend={backend:?}, rate={rate}, frames={}",
                step.frames()
            );
            terminal.extend_from_slice(&quantum[..step.frames() * CHANNELS]);
            if step.complete() {
                let completed = engine
                    .flush(&mut quantum)
                    .expect("a completed drain remains queryable");
                assert_eq!(completed.frames(), 0);
                assert!(completed.complete());
                assert!(
                    terminal_pattern_is_valid(&terminal, expected_frames),
                    "Q64 drain must preserve the complete ordered terminal PCM: backend={backend:?}, rate={rate}, drained={}",
                    terminal.len() / CHANNELS
                );
                return;
            }
        }

        panic!(
            "terminal drain must complete within its exact frame bound: backend={backend:?}, rate={rate}, expected={expected_frames}, drained={}",
            terminal.len() / CHANNELS
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn terminal_flush_reaches_each_new_rate_within_declared_latency(#[case] backend: StretchKind) {
        const OUTPUT_FRAMES: usize = 8192;
        const MAX_SOURCE_FRAMES: usize = OUTPUT_FRAMES * 2;
        for (initial_is_minimum, settled_is_minimum) in [(true, false), (false, true)] {
            let mut engine = prepared_backend_with_rate_envelope(
                backend,
                MAX_SOURCE_FRAMES,
                OUTPUT_FRAMES,
                0.5..=2.0,
            );
            let capabilities = engine.capabilities();
            let initial = edge_request(capabilities, OUTPUT_FRAMES, initial_is_minimum);
            let mut source_position = 0;
            for _ in 0..2 {
                let source = continuous_tone(initial.source_frames(), source_position);
                let mut output = vec![0.0; initial.output_frames() * CHANNELS];
                engine
                    .process(initial, &source, &mut output)
                    .expect("the initial rate reaches audible steady state");
                source_position += initial.source_frames();
            }

            let settled_output_frames = capabilities.latency().output_frames();
            let settled = edge_request(capabilities, settled_output_frames, settled_is_minimum);
            let settled_source = short_marker_signal(settled.source_frames());
            let mut settled_output = vec![0.0; settled.output_frames() * CHANNELS];
            engine
                .process(settled, &settled_source, &mut settled_output)
                .expect("the adjacent rate settles within its declared latency");

            let terminal = drain_terminal(engine.as_mut());
            let expected = rate_aware_latency_frames(capabilities, settled);
            let mut settled_and_terminal = settled_output;
            settled_and_terminal.extend_from_slice(&terminal);
            assert_eq!(
                terminal.len() / CHANNELS,
                expected,
                "terminal drain must reach the new rate in both directions: backend={backend:?}, initial={initial:?}, settled={settled:?}"
            );
            assert!(
                short_marker_is_present(&settled_and_terminal),
                "exactly one latency window must apply the new mapping and reach the terminal source marker: backend={backend:?}, initial={initial:?}, settled={settled:?}",
            );
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn terminal_flush_reaches_practical_rate_edges_after_declared_latency(
        #[case] backend: StretchKind,
    ) {
        const OUTPUT_FRAMES: usize = 8192;
        const MAX_SOURCE_FRAMES: usize = OUTPUT_FRAMES * 4;

        for (initial_is_minimum, settled_is_minimum) in [(true, false), (false, true)] {
            let mut engine = prepared_backend(backend, MAX_SOURCE_FRAMES, OUTPUT_FRAMES);
            let capabilities = engine.capabilities();
            let initial = edge_request(capabilities, OUTPUT_FRAMES, initial_is_minimum);
            let mut source_position = 0;
            for _ in 0..2 {
                let source = continuous_tone(initial.source_frames(), source_position);
                let mut output = vec![0.0; initial.output_frames() * CHANNELS];
                engine
                    .process(initial, &source, &mut output)
                    .expect("the practical initial edge reaches steady state");
                source_position += initial.source_frames();
            }

            let settled_output_frames = capabilities.latency().output_frames();
            let settled = edge_request(capabilities, settled_output_frames, settled_is_minimum);
            let source = continuous_tone(settled.source_frames(), source_position);
            let mut output = vec![0.0; settled.output_frames() * CHANNELS];
            engine
                .process(settled, &source, &mut output)
                .expect("the practical adjacent edge renders one latency window");

            let terminal = drain_terminal(engine.as_mut());
            let expected = rate_aware_latency_frames(capabilities, settled);
            assert_eq!(
                terminal.len() / CHANNELS,
                expected,
                "one declared latency window must settle the exact tail formula at both practical rate edges: backend={backend:?}, initial={initial:?}, settled={settled:?}"
            );
            assert!(
                terminal.iter().any(|sample| sample.abs() >= 1.0e-4),
                "the practical-edge tail must contain real source audio: backend={backend:?}, initial={initial:?}, settled={settled:?}"
            );
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn transitional_eof_preserves_every_indexed_marker(#[case] backend: StretchKind) {
        const OUTPUT_FRAMES: usize = 8192;
        const MAX_SOURCE_FRAMES: usize = OUTPUT_FRAMES * 2;
        const INITIAL_LANDMARKS: usize = 8;
        const LANDMARKS_PER_INITIAL_BLOCK: usize = 2;
        for initial_is_slow in [true, false] {
            let mut engine = prepared_backend(backend, MAX_SOURCE_FRAMES, OUTPUT_FRAMES);
            let capabilities = engine.capabilities();
            let initial_source_frames = if initial_is_slow {
                OUTPUT_FRAMES / 2
            } else {
                OUTPUT_FRAMES * 2
            };
            let initial = ElasticRequest::new(initial_source_frames, OUTPUT_FRAMES)
                .expect("the initial transition request is valid");
            let mut rendered = Vec::new();
            for start in (0..INITIAL_LANDMARKS).step_by(LANDMARKS_PER_INITIAL_BLOCK) {
                let end = start + LANDMARKS_PER_INITIAL_BLOCK;
                let landmarks = (start..end).collect::<Vec<_>>();
                let source = landmark_signal(initial.source_frames(), &landmarks);
                let mut output = vec![0.0; initial.output_frames() * CHANNELS];
                engine
                    .process(initial, &source, &mut output)
                    .expect("the indexed initial block renders");
                rendered.extend_from_slice(&output);
            }
            let transition_output_frames = capabilities.latency().output_frames() / 4;
            assert!(
                transition_output_frames > 0
                    && transition_output_frames < capabilities.latency().output_frames(),
                "the transition fixture must stop before convergence"
            );
            let transition_source_frames = if initial_is_slow {
                transition_output_frames * 2
            } else {
                transition_output_frames / 2
            };
            let transition =
                ElasticRequest::new(transition_source_frames, transition_output_frames)
                    .expect("the short transition request is valid");
            let source = short_marker_signal(transition.source_frames());
            let mut output = vec![0.0; transition.output_frames() * CHANNELS];
            engine
                .process(transition, &source, &mut output)
                .expect("the short adjacent-rate block renders");
            let mut transition_and_terminal = output;
            transition_and_terminal.extend_from_slice(&drain_terminal(engine.as_mut()));
            rendered.extend_from_slice(&transition_and_terminal);

            let expected = (0..INITIAL_LANDMARKS).collect::<Vec<_>>();
            assert!(
                landmarks_appear_once_in_order(&rendered, &expected),
                "transitional EOF must preserve every indexed source landmark exactly once and in order: backend={backend:?}, initial={initial:?}, transition={transition:?}, sequence={:?}",
                dominant_landmark_sequence(&rendered, &expected),
            );
            assert!(
                short_marker_is_present(&transition_and_terminal),
                "transitional EOF must preserve the short terminal source marker: backend={backend:?}, initial={initial:?}, transition={transition:?}"
            );
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn flush_rejects_partial_frame_storage_without_disarming_tail(#[case] backend: StretchKind) {
        const FRAMES: usize = 8192;

        let mut engine = prepared_backend(backend, FRAMES, FRAMES);
        let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
        let source = interleaved_signal(FRAMES);
        let mut output = vec![0.0; FRAMES * CHANNELS];
        engine
            .process(request, &source, &mut output)
            .expect("the source block renders");
        let mut partial_frame = vec![0.0; CONTROL_QUANTUM * CHANNELS - 1];

        let error = engine
            .flush(&mut partial_frame)
            .expect_err("an armed tail requires whole-frame storage");

        assert_eq!(
            error,
            ElasticError::OutputSampleCount {
                actual: partial_frame.len(),
                expected: CONTROL_QUANTUM * CHANNELS,
            }
        );
        let mut terminal = vec![0.0; CONTROL_QUANTUM * CHANNELS];
        let drained = engine
            .flush(&mut terminal)
            .expect("the rejected call keeps the tail armed");
        assert!(drained.frames() > 0);
        assert!(drained.frames() <= CONTROL_QUANTUM);
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn fresh_engine_has_no_terminal_tail(#[case] backend: StretchKind) {
        let mut engine = prepared_backend(backend, 8192, 8192);
        let mut terminal = vec![0.0; CONTROL_QUANTUM * CHANNELS];

        let step = engine.flush(&mut terminal).expect("fresh terminal drain");

        assert_eq!(step.frames(), 0);
        assert!(step.complete());
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn reset_engine_has_no_terminal_tail(#[case] backend: StretchKind) {
        const FRAMES: usize = 8192;

        let mut engine = prepared_backend(backend, FRAMES, FRAMES);
        let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
        let source = interleaved_signal(FRAMES);
        let mut output = vec![0.0; FRAMES * CHANNELS];
        engine
            .process(request, &source, &mut output)
            .expect("the source block renders");
        engine.reset().expect("the engine clears its history");
        let mut terminal = vec![0.0; CONTROL_QUANTUM * CHANNELS];

        let step = engine.flush(&mut terminal).expect("reset terminal drain");

        assert_eq!(step.frames(), 0);
        assert!(step.complete());
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn reset_clears_stream_history_without_changing_capabilities(#[case] backend: StretchKind) {
        const LONG_FRAMES: usize = 16_384;
        const SHORT_FRAMES: usize = 4096;

        let mut engine = prepared_backend(backend, LONG_FRAMES, LONG_FRAMES);
        let mut fresh = prepared_backend(backend, LONG_FRAMES, LONG_FRAMES);
        let capabilities = engine.capabilities();
        assert_eq!(fresh.capabilities(), capabilities);
        let source = interleaved_signal(LONG_FRAMES);
        let mut output = vec![0.0; LONG_FRAMES * CHANNELS];
        engine
            .process(
                ElasticRequest::new(LONG_FRAMES, LONG_FRAMES).expect("unity request"),
                &source,
                &mut output,
            )
            .expect("the warm request is supported");
        assert!(output.iter().any(|sample| sample.abs() > f32::EPSILON));

        engine.reset().expect("the engine clears its history");
        let request = ElasticRequest::new(SHORT_FRAMES, SHORT_FRAMES).expect("unity request");
        let short_source = &source[..SHORT_FRAMES * CHANNELS];
        let mut reset_output = vec![f32::NAN; SHORT_FRAMES * CHANNELS];
        let mut fresh_output = vec![f32::NAN; SHORT_FRAMES * CHANNELS];
        engine
            .process(request, short_source, &mut reset_output)
            .expect("the request after reset is supported");
        fresh
            .process(request, short_source, &mut fresh_output)
            .expect("the fresh reference request is supported");

        assert_eq!(engine.capabilities(), capabilities);
        assert_exact_samples(&reset_output, &fresh_output);
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn preserves_tone_pitch_when_source_advance_changes(#[case] backend: StretchKind) {
        const SOURCE_FRAMES: usize = 19_200;
        const OUTPUT_FRAMES: usize = 16_000;

        let config = ElasticConfig::builder()
            .backend(backend)
            .pools(default_pools())
            .sample_rate(SAMPLE_RATE)
            .channels(1)
            .max_source_frames(SOURCE_FRAMES)
            .max_output_frames(OUTPUT_FRAMES)
            .build()
            .expect("the test configuration is valid");
        let mut engine = build_engine(config).expect("the selected engine prepares");
        let request = ElasticRequest::new(SOURCE_FRAMES, OUTPUT_FRAMES).expect("non-empty request");
        let phase_step = TAU * 440.0 / 48_000.0;
        let mut phase: f32 = 0.0;
        let source = (0..SOURCE_FRAMES)
            .map(|_| {
                let sample = phase.sin();
                phase += phase_step;
                sample
            })
            .collect::<Vec<_>>();
        let mut output = vec![0.0; OUTPUT_FRAMES];

        engine
            .process(request, &source, &mut output)
            .expect("the request is supported");

        let latency = rate_aware_latency_frames(engine.capabilities(), request);
        let audible = output
            .len()
            .checked_sub(latency)
            .expect("the block must outlast the engine latency");
        let expected =
            TONE_HZ * audible.to_f64().expect("audible span fits in f64") / f64::from(SAMPLE_RATE);
        let positive_crossings = output[latency..]
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count()
            .to_f64()
            .expect("crossing count fits in f64");
        assert!(
            (positive_crossings - expected).abs() <= expected * 0.1,
            "expected a pitch-locked {TONE_HZ} Hz tone (~{expected} crossings), counted {positive_crossings}"
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn rate_envelope_is_the_configured_practical_domain(#[case] backend: StretchKind) {
        let engine = prepared_backend(backend, 8192, 4096);
        let envelope = engine.capabilities().rate_envelope();

        assert_eq!(envelope.min_source_frames_per_output(), 0.05);
        assert_eq!(envelope.max_source_frames_per_output(), 4.0);
        assert!(!envelope.contains_rate(envelope.min_source_frames_per_output() / 2.0));
        assert!(!envelope.contains_rate(envelope.max_source_frames_per_output() * 2.0));
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn rejects_buffers_that_do_not_match_the_request(#[case] backend: StretchKind) {
        let mut engine = prepared_backend(backend, 8192, 8192);
        let request = ElasticRequest::new(4800, 4000).expect("non-empty request");
        let source = interleaved_signal(request.source_frames());
        let mut output = vec![0.0; request.output_frames() * CHANNELS];

        assert_eq!(
            engine.process(request, &source[..source.len() - 1], &mut output),
            Err(ElasticError::SourceSampleCount {
                actual: source.len() - 1,
                expected: 4800 * CHANNELS,
            })
        );

        let output_len = output.len();
        assert_eq!(
            engine.process(request, &source, &mut output[..output_len - 1]),
            Err(ElasticError::OutputSampleCount {
                actual: output_len - 1,
                expected: 4000 * CHANNELS,
            })
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn rejects_spans_beyond_the_prepared_block_limits(#[case] backend: StretchKind) {
        const MAX_SOURCE_FRAMES: usize = 2048;
        const MAX_OUTPUT_FRAMES: usize = 2048;

        let mut engine = prepared_backend(backend, MAX_SOURCE_FRAMES, MAX_OUTPUT_FRAMES);
        let mut output = vec![0.0; MAX_OUTPUT_FRAMES * CHANNELS];
        let source = interleaved_signal(MAX_SOURCE_FRAMES + 1);

        let request = ElasticRequest::new(MAX_SOURCE_FRAMES + 1, MAX_OUTPUT_FRAMES)
            .expect("non-empty request");
        assert_eq!(
            engine.process(request, &source, &mut output),
            Err(ElasticError::SourceFrameLimit {
                frames: MAX_SOURCE_FRAMES + 1,
                limit: MAX_SOURCE_FRAMES,
            })
        );

        let mut long_output = vec![0.0; (MAX_OUTPUT_FRAMES + 1) * CHANNELS];
        let request = ElasticRequest::new(MAX_SOURCE_FRAMES, MAX_OUTPUT_FRAMES + 1)
            .expect("non-empty request");
        assert_eq!(
            engine.process(
                request,
                &source[..MAX_SOURCE_FRAMES * CHANNELS],
                &mut long_output,
            ),
            Err(ElasticError::OutputFrameLimit {
                frames: MAX_OUTPUT_FRAMES + 1,
                limit: MAX_OUTPUT_FRAMES,
            })
        );

        let request = ElasticRequest::new(32, 1).expect("non-empty extreme-rate request");
        assert_eq!(
            engine.process(
                request,
                &source[..request.source_frames() * CHANNELS],
                &mut output[..request.output_frames() * CHANNELS],
            ),
            Err(ElasticError::RateOutsideEnvelope {
                source_frames: 32,
                output_frames: 1,
            })
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn plans_and_renders_one_block_of_continuous_source_spans(#[case] backend: StretchKind) {
        use kithara_stretch::{ElasticSpan, ElasticSpanPlan};

        const OUTPUT_FRAMES: usize = 512;

        let mut engine = prepared_backend(backend, 4096, 4096);
        let capabilities = engine.capabilities();
        let span_config = ElasticSpanConfig::builder()
            .build()
            .expect("finite positive span policy");
        let source_end = OUTPUT_FRAMES
            .to_f64()
            .expect("invariant: the block fits in f64");
        let plan = ElasticSpanPlan::new(
            [
                ElasticSpan::try_from((0.0..source_end / 2.0, OUTPUT_FRAMES / 2))
                    .expect("first continuous span"),
                ElasticSpan::try_from((source_end / 2.0..source_end, OUTPUT_FRAMES / 2))
                    .expect("second continuous span"),
            ],
            None,
            capabilities,
            span_config,
        )
        .expect("a unity path is inside every declared envelope");

        let source = interleaved_signal(OUTPUT_FRAMES);
        let mut consumed = 0;
        for segment in plan.segments() {
            let request = segment.request();
            let samples = request.source_frames() * CHANNELS;
            let mut output = vec![f32::NAN; request.output_frames() * CHANNELS];

            engine
                .process(request, &source[consumed..consumed + samples], &mut output)
                .expect("a planned segment is always renderable");

            assert!(output.iter().all(|sample| sample.is_finite()));
            consumed += samples;
        }
        assert_eq!(
            plan.cursor().integer(),
            i64::try_from(OUTPUT_FRAMES).expect("cursor fits")
        );
    }
}

mod priming {
    use super::*;

    type PrimedPair = (
        Box<dyn ElasticEngine>,
        Box<dyn ElasticEngine>,
        ElasticCapabilities,
        usize,
    );

    fn indexed_markers(frames: usize, offset: usize) -> Vec<f32> {
        marker_signal(frames, offset, |index| {
            let marker_index = u16::try_from(index.wrapping_mul(73) % 997)
                .expect("invariant: marker index is bounded below 997");
            (f32::from(marker_index) / 997.0) * 1.5 - 0.75
        })
    }

    fn mean(samples: &[f32]) -> f32 {
        samples.iter().sum::<f32>()
            / samples
                .len()
                .to_f32()
                .expect("the sample window fits in f32")
    }

    fn warmup_request(
        capabilities: ElasticCapabilities,
        source_frames_per_output: f64,
    ) -> ElasticRequest {
        assert!(
            capabilities
                .rate_envelope()
                .contains_rate(source_frames_per_output),
            "invariant: warmup rate stays inside the envelope"
        );
        let output_frames = capabilities.latency().output_frames();
        let source_frames = source_frames_at(source_frames_per_output, output_frames, false);
        ElasticRequest::new(source_frames, output_frames)
            .expect("invariant: warmup request is valid")
    }

    fn primed_playing_pair(backend: StretchKind) -> PrimedPair {
        const MAX_FRAMES: usize = 65_536;

        let mut reference = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
        let mut changed = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
        let capabilities = reference.capabilities();
        assert_eq!(changed.capabilities(), capabilities);
        let latency = capabilities.latency();
        let warmup = warmup_request(capabilities, 1.0);
        let history = indexed_markers(latency.source_frames(), 0);
        let lookahead = indexed_markers(latency.source_frames(), latency.source_frames());
        let warm_source = indexed_markers(
            warmup.source_frames(),
            latency.source_frames().saturating_mul(2),
        );
        let mut reference_discard = vec![0.0; warmup.output_frames() * CHANNELS];
        let mut changed_discard = vec![0.0; warmup.output_frames() * CHANNELS];
        reference
            .prime(
                warmup,
                &history,
                &lookahead,
                &warm_source,
                &mut reference_discard,
            )
            .expect("reference engine primes");
        changed
            .prime(
                warmup,
                &history,
                &lookahead,
                &warm_source,
                &mut changed_discard,
            )
            .expect("changed engine primes");

        let continuation = latency
            .source_frames()
            .saturating_mul(2)
            .saturating_add(warmup.source_frames());
        let source = indexed_markers(CONTROL_QUANTUM, continuation);
        let request = ElasticRequest::new(CONTROL_QUANTUM, CONTROL_QUANTUM)
            .expect("lead quantum is non-empty");
        let mut reference_output = vec![f32::NAN; CONTROL_QUANTUM * CHANNELS];
        let mut changed_output = vec![f32::NAN; CONTROL_QUANTUM * CHANNELS];
        reference
            .process(request, &source, &mut reference_output)
            .expect("reference lead quantum renders");
        changed
            .process(request, &source, &mut changed_output)
            .expect("changed lead quantum renders");
        assert_exact_samples(&changed_output, &reference_output);

        (
            reference,
            changed,
            capabilities,
            continuation + CONTROL_QUANTUM,
        )
    }

    fn assert_control_response(
        reference: &mut dyn ElasticEngine,
        changed: &mut dyn ElasticEngine,
        capabilities: ElasticCapabilities,
        continuation: usize,
        changed_rate: f64,
        changed_pitch: f64,
    ) {
        changed
            .set_pitch(changed_pitch)
            .expect("the changed pitch is supported");
        let mut reference_position = continuation;
        let mut changed_position = continuation;
        let mut remaining = capabilities.latency().output_frames();
        while remaining > 0 {
            let output_frames = remaining.min(CONTROL_QUANTUM);
            let reference_request = ElasticRequest::new(output_frames, output_frames)
                .expect("reference quantum is non-empty");
            let changed_source_frames = source_frames_at(changed_rate, output_frames, false);
            let changed_request = ElasticRequest::new(changed_source_frames, output_frames)
                .expect("changed quantum is non-empty");
            let reference_source = indexed_markers(output_frames, reference_position);
            let changed_source = indexed_markers(changed_source_frames, changed_position);
            let mut reference_output = vec![f32::NAN; output_frames * CHANNELS];
            let mut changed_output = vec![f32::NAN; output_frames * CHANNELS];
            reference
                .process(reference_request, &reference_source, &mut reference_output)
                .expect("reference control quantum renders");
            changed
                .process(changed_request, &changed_source, &mut changed_output)
                .expect("changed control quantum renders");
            assert!(reference_output.iter().all(|sample| sample.is_finite()));
            assert!(changed_output.iter().all(|sample| sample.is_finite()));
            if changed_output != reference_output {
                return;
            }
            reference_position += output_frames;
            changed_position += changed_source_frames;
            remaining -= output_frames;
        }

        panic!("a control change must affect output within the declared native latency");
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn history_and_output_warmup_remove_the_initial_gap(#[case] backend: StretchKind) {
        const FRAMES: usize = 512;

        let mut engine = prepared_backend(backend, FRAMES * 2, FRAMES);
        let capabilities = engine.capabilities();
        let history_frames = capabilities.latency().source_frames();
        let history = vec![0.25; history_frames * CHANNELS];
        let lookahead = vec![0.25; history.len()];
        let warmup = warmup_request(capabilities, 1.0);
        let warm_source = vec![0.25; warmup.source_frames() * CHANNELS];
        let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
        engine
            .prime(warmup, &history, &lookahead, &warm_source, &mut discarded)
            .expect("history and output latency warmup");
        let source = vec![0.25; FRAMES * CHANNELS];
        let mut output = vec![0.0; FRAMES * CHANNELS];

        engine
            .process(
                ElasticRequest::new(FRAMES, FRAMES).expect("unity request"),
                &source,
                &mut output,
            )
            .expect("primed unity request");

        assert_eq!(first_audible_frame(&output, CHANNELS), Some(0));
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn post_prime_pitch_change_responds_within_declared_latency(#[case] backend: StretchKind) {
        let (mut reference, mut changed, capabilities, continuation) = primed_playing_pair(backend);

        assert_control_response(
            reference.as_mut(),
            changed.as_mut(),
            capabilities,
            continuation,
            1.0,
            1.5,
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn post_prime_rate_change_responds_within_declared_latency(#[case] backend: StretchKind) {
        let (mut reference, mut changed, capabilities, continuation) = primed_playing_pair(backend);

        assert_control_response(
            reference.as_mut(),
            changed.as_mut(),
            capabilities,
            continuation,
            2.0,
            1.0,
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn repeated_adjacent_rate_and_pitch_corrections_remain_continuous(
        #[case] backend: StretchKind,
    ) {
        const MAX_FRAMES: usize = 65_536;
        const TRANSITIONS: usize = 32;

        let mut engine = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
        let capabilities = engine.capabilities();
        let latency = capabilities.latency();
        let warmup = warmup_request(capabilities, 1.0);
        let history = continuous_tone(latency.source_frames(), 0);
        let lookahead = continuous_tone(latency.source_frames(), latency.source_frames());
        let warm_offset = latency.source_frames().saturating_mul(2);
        let warm_source = continuous_tone(warmup.source_frames(), warm_offset);
        let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
        engine
            .prime(warmup, &history, &lookahead, &warm_source, &mut discarded)
            .expect("the continuous fixture primes at unity");

        let mut source_position = warm_offset.saturating_add(warmup.source_frames());
        let mut previous: Option<[f32; CHANNELS]> = None;
        for transition in 0..TRANSITIONS {
            let corrected = transition.is_multiple_of(2);
            let source_frames = if corrected {
                CONTROL_QUANTUM * 2
            } else {
                CONTROL_QUANTUM
            };
            let pitch = if corrected { 1.5 } else { 1.0 };
            engine
                .set_pitch(pitch)
                .expect("every correction stays inside the common pitch range");
            let source = continuous_tone(source_frames, source_position);
            let mut output = vec![f32::NAN; CONTROL_QUANTUM * CHANNELS];
            engine
                .process(
                    ElasticRequest::new(source_frames, CONTROL_QUANTUM)
                        .expect("the adjacent correction is non-empty"),
                    &source,
                    &mut output,
                )
                .expect("the adjacent correction renders through the public facade");

            assert!(
                output.iter().all(|sample| sample.is_finite()),
                "transition {transition} produced a non-finite sample"
            );
            assert!(
                output.iter().any(|sample| sample.abs() > f32::EPSILON),
                "the primed fixture must remain audible at transition {transition}"
            );
            if let Some(previous) = previous {
                for channel in 0..CHANNELS {
                    let step = (output[channel] - previous[channel]).abs();
                    assert!(
                        step <= 0.1,
                        "transition {transition} clicked on channel {channel}: step={step}, backend={backend:?}"
                    );
                }
            }
            previous = Some([
                output[(CONTROL_QUANTUM - 1) * CHANNELS],
                output[CONTROL_QUANTUM * CHANNELS - 1],
            ]);
            source_position = source_position.saturating_add(source_frames);
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn source_history_conditions_the_cue_boundary(#[case] backend: StretchKind) {
        const MAX_FRAMES: usize = 65_536;

        let mut conditioned = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
        let mut zero_padded = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
        let capabilities = conditioned.capabilities();
        assert_eq!(zero_padded.capabilities(), capabilities);
        let latency = capabilities.latency();
        let warmup = warmup_request(capabilities, 1.0);
        let history = continuous_tone(latency.source_frames(), 0);
        let empty_history = vec![0.0; history.len()];
        let lookahead = continuous_tone(latency.source_frames(), latency.source_frames());
        let warm_source = continuous_tone(
            warmup.source_frames(),
            latency.source_frames().saturating_mul(2),
        );
        let mut conditioned_discard = vec![0.0; warmup.output_frames() * CHANNELS];
        let mut zero_padded_discard = vec![0.0; warmup.output_frames() * CHANNELS];
        conditioned
            .prime(
                warmup,
                &history,
                &lookahead,
                &warm_source,
                &mut conditioned_discard,
            )
            .expect("conditioned engine primes");
        zero_padded
            .prime(
                warmup,
                &empty_history,
                &lookahead,
                &warm_source,
                &mut zero_padded_discard,
            )
            .expect("zero-padded engine primes");

        let quantum = latency.output_frames();
        let source = continuous_tone(
            quantum,
            latency
                .source_frames()
                .saturating_mul(2)
                .saturating_add(warmup.source_frames()),
        );
        let request = ElasticRequest::new(quantum, quantum).expect("next quantum is non-empty");
        let mut conditioned_output = vec![f32::NAN; quantum * CHANNELS];
        let mut zero_padded_output = vec![f32::NAN; quantum * CHANNELS];
        conditioned
            .process(request, &source, &mut conditioned_output)
            .expect("conditioned next quantum renders");
        zero_padded
            .process(request, &source, &mut zero_padded_output)
            .expect("zero-padded next quantum renders");

        assert!(conditioned_output.iter().all(|sample| sample.is_finite()));
        assert!(zero_padded_output.iter().all(|sample| sample.is_finite()));
        assert!(
            conditioned_output != zero_padded_output,
            "pre-cue history must condition the cue boundary without becoming audible source"
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_slow(StretchKind::Signalsmith, 0.05)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_fast(StretchKind::Signalsmith, 4.0)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_slow(StretchKind::Bungee, 0.05)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_fast(StretchKind::Bungee, 4.0)
    )]
    fn prime_accepts_declared_rate_edges(#[case] backend: StretchKind, #[case] rate: f64) {
        const FRAMES: usize = 512;

        let mut engine = prepared_backend(backend, FRAMES * 2, FRAMES);
        let capabilities = engine.capabilities();
        let history_frames = capabilities.latency().source_frames();
        let output_frames = capabilities.latency().output_frames();
        let source_frames = source_frames_at(rate, output_frames, rate < 1.0);
        let request = ElasticRequest::new(source_frames, output_frames)
            .expect("the declared edge request is non-empty");
        let history = vec![0.25; history_frames * CHANNELS];
        let lookahead = vec![0.25; history.len()];
        let source = vec![0.25; source_frames * CHANNELS];
        let mut discarded = vec![f32::NAN; output_frames * CHANNELS];

        engine
            .prime(request, &history, &lookahead, &source, &mut discarded)
            .expect("the declared prime rate edge is supported");

        assert!(discarded.iter().all(|sample| sample.is_finite()));
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_unity(StretchKind::Signalsmith, 1.0)
    )]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith_non_unity(StretchKind::Signalsmith, 1.2)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_unity(StretchKind::Bungee, 1.0)
    )]
    #[cfg_attr(
        feature = "stretch-bungee",
        case::bungee_non_unity(StretchKind::Bungee, 1.2)
    )]
    fn priming_hides_history_and_preserves_source_order(
        #[case] backend: StretchKind,
        #[case] source_frames_per_output: f64,
    ) {
        const MAX_FRAMES: usize = 65_536;
        const FOLLOWING_FRAMES: usize = 4096;

        let mut engine = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
        let capabilities = engine.capabilities();
        let history_frames = capabilities.latency().source_frames();
        let warmup = warmup_request(capabilities, source_frames_per_output);
        let history = vec![0.9; history_frames * CHANNELS];
        let lookahead = vec![0.2; history.len()];
        let warm_source = vec![0.5; warmup.source_frames() * CHANNELS];
        let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
        engine
            .prime(warmup, &history, &lookahead, &warm_source, &mut discarded)
            .expect("the engine absorbs the complete preroll");

        let render_output_frames = warmup
            .output_frames()
            .checked_mul(3)
            .and_then(|frames| frames.checked_add(FOLLOWING_FRAMES))
            .expect("the continuation span fits in usize");
        let render_source_frames =
            source_frames_at(source_frames_per_output, render_output_frames, false);
        assert!(render_source_frames <= MAX_FRAMES);
        assert!(render_output_frames <= MAX_FRAMES);
        let source = vec![0.8; render_source_frames * CHANNELS];
        let mut output = vec![f32::NAN; render_output_frames * CHANNELS];
        engine
            .process(
                ElasticRequest::new(render_source_frames, render_output_frames)
                    .expect("continuation request"),
                &source,
                &mut output,
            )
            .expect("the primed stream continues");

        let lookahead_output_frames = history_frames
            .to_f64()
            .map(|frames| (frames / source_frames_per_output).round())
            .and_then(|frames| frames.to_usize())
            .expect("the lookahead output span fits in usize");
        let lookahead_begin = lookahead_output_frames / 4;
        let lookahead_end = lookahead_output_frames * 3 / 4;
        let lookahead_mean = mean(&output[lookahead_begin * CHANNELS..lookahead_end * CHANNELS]);
        let warm_begin = lookahead_output_frames + warmup.output_frames() / 4;
        let warm_end = lookahead_output_frames + warmup.output_frames() * 3 / 4;
        let warm_mean = mean(&output[warm_begin * CHANNELS..warm_end * CHANNELS]);
        let following_begin = render_output_frames - FOLLOWING_FRAMES / 2;
        let following_mean = mean(&output[following_begin * CHANNELS..]);

        assert!(
            lookahead_mean.is_finite() && lookahead_mean > 0.01,
            "the post-cue lookahead must be audible, mean={lookahead_mean}"
        );
        assert!(
            lookahead_mean + 0.02 < warm_mean,
            "pre-cue history leaked or the warmer region was skipped: lookahead={lookahead_mean}, warm={warm_mean}"
        );
        assert!(
            warm_mean + 0.1 < following_mean,
            "the warmup was duplicated or following source was skipped: warm={warm_mean}, following={following_mean}"
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn prime_rejects_every_ambiguous_buffer_count(#[case] backend: StretchKind) {
        let mut engine = prepared_backend(backend, 1024, 512);
        let capabilities = engine.capabilities();
        let warmup = warmup_request(capabilities, 1.0);
        let history = vec![0.25; capabilities.latency().source_frames() * CHANNELS];
        let lookahead = vec![0.25; history.len()];
        let source = vec![0.25; warmup.source_frames() * CHANNELS];
        let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];

        assert_eq!(
            engine.prime(
                warmup,
                &history[..history.len() - 1],
                &lookahead,
                &source,
                &mut discarded
            ),
            Err(ElasticError::HistorySampleCount {
                actual: history.len() - 1,
                expected: history.len(),
            })
        );
        assert_eq!(
            engine.prime(
                warmup,
                &history,
                &lookahead[..lookahead.len() - 1],
                &source,
                &mut discarded,
            ),
            Err(ElasticError::LookaheadSampleCount {
                actual: lookahead.len() - 1,
                expected: lookahead.len(),
            })
        );
        assert_eq!(
            engine.prime(
                warmup,
                &history,
                &lookahead,
                &source[..source.len() - 1],
                &mut discarded
            ),
            Err(ElasticError::SourceSampleCount {
                actual: source.len() - 1,
                expected: source.len(),
            })
        );
        let discarded_len = discarded.len();
        assert_eq!(
            engine.prime(
                warmup,
                &history,
                &lookahead,
                &source,
                &mut discarded[..discarded_len - 1]
            ),
            Err(ElasticError::OutputSampleCount {
                actual: discarded_len - 1,
                expected: discarded_len,
            })
        );
        let wrong_output = ElasticRequest::new(warmup.source_frames(), warmup.output_frames() - 1)
            .expect("non-empty mismatched warmup request");
        assert_eq!(
            engine.prime(wrong_output, &history, &lookahead, &source, &mut discarded,),
            Err(ElasticError::WarmupOutputFrameCount {
                actual: warmup.output_frames() - 1,
                expected: warmup.output_frames(),
            })
        );
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn reset_reprime_keeps_the_first_frame_aligned(#[case] backend: StretchKind) {
        const SOURCE_FRAMES: usize = 600;
        const OUTPUT_FRAMES: usize = 500;

        let mut engine = prepared_backend(backend, SOURCE_FRAMES, OUTPUT_FRAMES);
        let capabilities = engine.capabilities();
        let warmup = warmup_request(capabilities, 1.2);
        let history = vec![0.25; capabilities.latency().source_frames() * CHANNELS];
        let lookahead = vec![0.25; history.len()];
        let warm_source = vec![0.25; warmup.source_frames() * CHANNELS];
        let source = vec![0.25; SOURCE_FRAMES * CHANNELS];
        let request = ElasticRequest::new(SOURCE_FRAMES, OUTPUT_FRAMES).expect("non-unity request");
        let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
        let mut output = vec![0.0; OUTPUT_FRAMES * CHANNELS];

        for cycle in 0..8 {
            if cycle > 0 {
                engine.reset().expect("the engine clears its history");
            }
            engine
                .prime(warmup, &history, &lookahead, &warm_source, &mut discarded)
                .expect("reset engine primes again");
            engine
                .process(request, &source, &mut output)
                .expect("request after reset is supported");

            assert_eq!(engine.capabilities(), capabilities);
            assert!(output[..CHANNELS].iter().all(|sample| sample.is_finite()));
            assert!(
                output[..CHANNELS]
                    .iter()
                    .any(|sample| sample.abs() > f32::EPSILON),
                "cycle {cycle} retained stale latency"
            );
        }
    }

    #[kithara::test]
    #[cfg_attr(
        feature = "stretch-signalsmith",
        case::signalsmith(StretchKind::Signalsmith)
    )]
    #[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
    fn prime_discards_previous_stream_state(#[case] backend: StretchKind) {
        const FRAMES: usize = 4096;

        let mut fresh = prepared_backend(backend, FRAMES, FRAMES);
        let mut reused = prepared_backend(backend, FRAMES, FRAMES);
        let capabilities = fresh.capabilities();
        let warmup = warmup_request(capabilities, 1.0);
        let history_frames = capabilities.latency().source_frames();
        let history = indexed_markers(history_frames, 0);
        let lookahead = indexed_markers(history_frames, history_frames);
        let warm_source = indexed_markers(warmup.source_frames(), history_frames * 2);
        let source = indexed_markers(FRAMES, history_frames * 2 + warmup.source_frames());
        let dirty_source = interleaved_signal(FRAMES);
        let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
        let mut dirty_output = vec![0.0; FRAMES * CHANNELS];
        reused
            .process(request, &dirty_source, &mut dirty_output)
            .expect("the dirtying request is supported");

        let mut fresh_discarded = vec![0.0; warmup.output_frames() * CHANNELS];
        let mut reused_discarded = vec![0.0; warmup.output_frames() * CHANNELS];
        fresh
            .prime(
                warmup,
                &history,
                &lookahead,
                &warm_source,
                &mut fresh_discarded,
            )
            .expect("fresh engine primes");
        reused
            .prime(
                warmup,
                &history,
                &lookahead,
                &warm_source,
                &mut reused_discarded,
            )
            .expect("reused engine primes");

        let mut fresh_output = vec![0.0; FRAMES * CHANNELS];
        let mut reused_output = vec![0.0; FRAMES * CHANNELS];
        fresh
            .process(request, &source, &mut fresh_output)
            .expect("fresh engine renders after priming");
        reused
            .process(request, &source, &mut reused_output)
            .expect("reused engine renders after priming");

        assert_exact_samples(&reused_discarded, &fresh_discarded);
        assert_exact_samples(&reused_output, &fresh_output);
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith, 208, 208)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee(StretchKind::Bungee, 256, 896)
)]
fn backend_declares_its_prepared_domain_and_latency(
    #[case] backend: StretchKind,
    #[case] expected_source_latency: usize,
    #[case] expected_output_latency: usize,
) {
    let engine = prepared_backend(backend, 8192, 8192);
    let capabilities = engine.capabilities();

    assert_eq!(capabilities.sample_rate(), SAMPLE_RATE);
    assert_eq!(capabilities.channels(), CHANNELS);
    assert_eq!(
        capabilities.rate_envelope().min_source_frames_per_output(),
        0.05
    );
    assert_eq!(
        capabilities.rate_envelope().max_source_frames_per_output(),
        4.0
    );
    assert_eq!(
        capabilities.latency().source_frames(),
        expected_source_latency
    );
    assert_eq!(
        capabilities.latency().output_frames(),
        expected_output_latency
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn unprimed_render_exposes_the_declared_total_latency(#[case] backend: StretchKind) {
    const FRAMES: usize = 65_536;

    let mut engine = prepared_backend(backend, FRAMES, FRAMES);
    let latency = engine.capabilities().latency();
    assert!(
        latency.source_frames() + latency.output_frames() < FRAMES,
        "the fixture must outlast the complete declared latency"
    );
    let source = impulse_markers(FRAMES, 0);
    let mut output = vec![f32::NAN; FRAMES * CHANNELS];

    engine
        .process(
            ElasticRequest::new(FRAMES, FRAMES).expect("unity request"),
            &source,
            &mut output,
        )
        .expect("unity is inside the supported envelope");

    let expected_first_audible = latency
        .source_frames()
        .checked_add(latency.output_frames())
        .expect("the declared latency fits usize");
    assert_eq!(
        first_audible_frame(&output, CHANNELS),
        Some(expected_first_audible),
        "the measured startup latency changed for {backend:?}: declared={latency:?}"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn prepare_uses_the_injected_pool_region_budget(#[case] backend: StretchKind) {
    let config = ElasticConfig::builder()
        .backend(backend)
        .pools(pools(0))
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .max_source_frames(8192)
        .max_output_frames(8192)
        .build()
        .expect("the numeric preparation shape is valid");

    let Err(error) = build_engine(config) else {
        panic!("zero region budget cannot prepare resident sample scratch");
    };

    assert_eq!(error, ElasticError::PoolCapacity);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn config_rejects_channels_outside_audio_spec_range(#[case] backend: StretchKind) {
    let channels = usize::from(u16::MAX) + 1;

    let result = ElasticConfig::builder()
        .backend(backend)
        .pools(default_pools())
        .sample_rate(SAMPLE_RATE)
        .channels(channels)
        .max_source_frames(CONTROL_QUANTUM)
        .max_output_frames(CONTROL_QUANTUM)
        .build();

    assert!(matches!(
        result,
        Err(ElasticError::ChannelCountOutOfRange(actual)) if actual == channels
    ));
}

#[cfg(feature = "stretch-bungee")]
#[kithara::test]
fn bungee_pool_usage_scales_with_the_prepared_source_limit() {
    fn allocated_bytes(max_source_frames: usize) -> usize {
        let pools = pools(usize::MAX);
        let config = ElasticConfig::builder()
            .backend(StretchKind::Bungee)
            .pools(pools.clone())
            .sample_rate(SAMPLE_RATE)
            .channels(CHANNELS)
            .max_source_frames(max_source_frames)
            .max_output_frames(8192)
            .build()
            .expect("the numeric preparation shape is valid");
        let engine = build_engine(config).expect("the prepared shape fits an unlimited pool");
        let allocated = pools.stats().allocated_bytes;
        drop(engine);
        allocated
    }

    let one_frame = allocated_bytes(1);
    let full_block = allocated_bytes(8192);

    assert!(
        one_frame < full_block,
        "latency probing must not inflate every shape to an 8192-frame allocation"
    );
}
