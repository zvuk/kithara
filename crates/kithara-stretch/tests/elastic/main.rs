//! Conformance suite for native keylock exact-span engines.
//!
//! Every compiled-in keylock engine runs the same conformance cases, including the
//! mandatory priming lifecycle.
//! Every observable lifecycle and audio behavior is shared; backend-specific
//! tests cover only private preparation and storage mechanics.

#![cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]

use std::{num::NonZeroUsize, ops::RangeInclusive};

use kithara_stretch::{
    BungeeConfig, ElasticBackendConfig, ElasticCapabilities, ElasticConfig, ElasticEngine,
    ElasticError, ElasticRequest, ElasticSpanConfig, SignalsmithConfig, StretchKind, build_engine,
};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::stretch_fixtures::{StretchPcm, stretch_pcm};
use kithara_test_utils::{
    bufpool::{pools as default_pools, pools_with_budget as pools},
    kithara,
};
use num_traits::ToPrimitive;

const CHANNELS: usize = 2;
const CONTROL_QUANTUM: usize = 64;
const SAMPLE_RATE: u32 = 48_000;
const TONE_HZ: f64 = 440.0;
const TERMINAL_WINDOW_FRAMES: usize = 64;

fn conformance_backends() -> ElasticBackendConfig {
    let signalsmith = SignalsmithConfig::builder()
        .block_frames(NonZeroUsize::new(416).expect("fixture block is non-zero"))
        .interval_frames(NonZeroUsize::new(64).expect("fixture interval is non-zero"))
        .build()
        .expect("valid Signalsmith geometry");
    let bungee = BungeeConfig::builder()
        .log2_synthesis_hop_adjust(-3)
        .build();
    ElasticBackendConfig::builder()
        .signalsmith(signalsmith)
        .bungee(bungee)
        .build()
}

fn prepared_backend(
    backend: StretchKind,
    max_source_frames: usize,
    max_output_frames: usize,
) -> Box<dyn ElasticEngine> {
    let config = ElasticConfig::builder()
        .backend(backend)
        .backends(conformance_backends())
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
        .backends(conformance_backends())
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

fn interleaved_signal(pcm: &StretchPcm, frames: usize) -> Vec<f32> {
    pcm.square[..frames * CHANNELS].to_vec()
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

fn impulse_markers(pcm: &StretchPcm, frames: usize, offset: usize) -> Vec<f32> {
    pcm.impulses[offset * CHANNELS..(offset + frames) * CHANNELS].to_vec()
}

fn continuous_tone(pcm: &StretchPcm, frames: usize, offset: usize) -> Vec<f32> {
    pcm.continuous[offset * CHANNELS..(offset + frames) * CHANNELS].to_vec()
}

fn landmark_signal(pcm: &StretchPcm, frames: usize, landmarks: &[usize]) -> Vec<f32> {
    assert!(!landmarks.is_empty());
    assert!(frames >= landmarks.len());
    (0..frames)
        .flat_map(|frame| {
            let slot = frame * landmarks.len() / frames;
            let slot_start = slot * frames / landmarks.len();
            let slot_end = (slot + 1) * frames / landmarks.len();
            let slot_frames = slot_end - slot_start;
            let position = frame - slot_start;
            let guarded =
                position < slot_frames / 8 || position >= slot_frames.saturating_mul(7) / 8;
            let tone = if guarded {
                &pcm.tones[12]
            } else {
                &pcm.tones[landmarks[slot]]
            };
            tone[position * CHANNELS..(position + 1) * CHANNELS]
                .iter()
                .copied()
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

fn terminal_marker_signal_with_span(
    pcm: &StretchPcm,
    frames: usize,
    marker_span: usize,
) -> Vec<f32> {
    let marker_frames = frames.min(marker_span);
    let marker_start = frames - marker_frames;
    let mut source = pcm.silence[..marker_start * CHANNELS].to_vec();
    let midpoint = marker_frames / 2;
    source.extend_from_slice(&pcm.tones[13][..midpoint * CHANNELS]);
    source.extend_from_slice(&pcm.tones[14][midpoint * CHANNELS..marker_frames * CHANNELS]);
    source
}

fn short_marker_signal(pcm: &StretchPcm, frames: usize) -> Vec<f32> {
    pcm.short[..frames * CHANNELS].to_vec()
}

fn short_marker_is_present(samples: &[f32]) -> bool {
    const SHORT_MARKER_HZ: f64 = 15_000.0;

    const SHORT_MARKER_WINDOW_FRAMES: usize = 16;

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
    const LANDMARK_FREQUENCIES: [f64; 12] = [
        1_125.0, 1_875.0, 2_625.0, 3_375.0, 4_125.0, 4_875.0, 5_625.0, 6_375.0, 7_125.0, 7_875.0,
        8_625.0, 9_375.0,
    ];

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
fn indexed_landmark_oracle_rejects_reorder_omission_replay_and_partial_drop(
    stretch_pcm: &'static StretchPcm,
) {
    const MARKER_FRAMES: usize = 2_048;

    let fixture = |order: &[usize]| {
        let mut samples = Vec::new();
        for &landmark in order {
            samples.extend_from_slice(&landmark_signal(stretch_pcm, MARKER_FRAMES, &[landmark]));
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

    let short = short_marker_signal(stretch_pcm, CONTROL_QUANTUM);
    assert!(short_marker_is_present(&short));
    assert!(!short_marker_is_present(
        &stretch_pcm.silence[..short.len()]
    ));
    assert!(!short_marker_is_present(&continuous_tone(
        stretch_pcm,
        CONTROL_QUANTUM,
        0
    )));
}

fn terminal_markers_are_ordered(samples: &[f32]) -> bool {
    const TERMINAL_HIGH_HZ: f64 = 6_000.0;
    const TERMINAL_LOW_HZ: f64 = 1_500.0;
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
        .first()
        .to_f64()
        .map(|frames| (frames / (source_frames / output_frames)).ceil())
        .and_then(|frames| frames.to_usize())
        .and_then(|frames| frames.checked_add(capabilities.latency().second()))
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
        .first()
        .checked_add(source_frames_at(
            rate,
            capabilities.latency().second(),
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

mod facade;
mod priming;

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
    assert_eq!(capabilities.latency().first(), expected_source_latency);
    assert_eq!(capabilities.latency().second(), expected_output_latency);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn unprimed_render_exposes_the_declared_total_latency(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const FRAMES: usize = 65_536;

    let mut engine = prepared_backend(backend, FRAMES, FRAMES);
    let latency = engine.capabilities().latency();
    assert!(
        latency.first() + latency.second() < FRAMES,
        "the fixture must outlast the complete declared latency"
    );
    let source = impulse_markers(stretch_pcm, FRAMES, 0);
    let mut output = vec![f32::NAN; FRAMES * CHANNELS];

    engine
        .process(
            ElasticRequest::new(FRAMES, FRAMES).expect("unity request"),
            &source,
            &mut output,
        )
        .expect("unity is inside the supported envelope");

    let expected_first_audible = latency
        .first()
        .checked_add(latency.second())
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
        .backends(conformance_backends())
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
        .backends(conformance_backends())
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
            .backends(conformance_backends())
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
