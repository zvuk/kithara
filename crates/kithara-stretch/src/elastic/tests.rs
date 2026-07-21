use std::f32::consts::TAU;

use kithara_test_utils::kithara;

use crate::{ElasticConfig, ElasticError, ElasticRequest, SignalsmithBackend};

const CHANNELS: usize = 2;

fn interleaved_signal(frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let sample = if frame % 64 < 32 { 0.25 } else { -0.25 };
            [sample, -sample]
        })
        .collect()
}

fn indexed_markers(frames: usize, offset: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let index = offset.wrapping_add(frame);
            let marker_index = u16::try_from(index.wrapping_mul(73) % 997)
                .expect("marker index is bounded below 997");
            let marker = (f32::from(marker_index) / 997.0) * 1.5 - 0.75;
            [marker, marker * -0.5]
        })
        .collect()
}

fn impulse_markers(frames: usize, offset: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let index = offset.wrapping_add(frame);
            let marker = if index.is_multiple_of(64) {
                let marker_index =
                    u16::try_from((index / 64) % 7).expect("marker index is bounded below 7");
                0.5 + f32::from(marker_index) / 14.0
            } else {
                0.0
            };
            [marker, marker * -0.5]
        })
        .collect()
}

fn first_audible_frame(samples: &[f32], channels: usize) -> Option<usize> {
    samples
        .chunks_exact(channels)
        .position(|frame| frame.iter().any(|sample| sample.abs() >= 1.0e-4))
}

fn assert_exact_samples(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual, expected, "sample {index} differs");
    }
}

#[kithara::test]
fn signalsmith_declares_reverse_input_support() {
    let config =
        ElasticConfig::new(48_000, CHANNELS, 512, 512).expect("the test configuration is valid");
    let backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");

    assert!(backend.capabilities().supports_reverse());
}

#[kithara::test]
fn renders_the_requested_output_frame_count() {
    let config =
        ElasticConfig::new(48_000, CHANNELS, 8192, 8192).expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let request = ElasticRequest::new(4800, 4000).expect("the request is non-empty");
    let source = interleaved_signal(request.source_frames());
    let mut output = vec![f32::NAN; request.output_frames() * CHANNELS];

    backend
        .process(request, &source, &mut output)
        .expect("the request is inside the prepared envelope");

    assert_eq!(output.len(), 4000 * CHANNELS);
    assert!(output.iter().all(|sample| sample.is_finite()));
    assert!(output.iter().any(|sample| sample.abs() > f32::EPSILON));
}

#[kithara::test]
fn unity_render_exposes_the_declared_source_and_output_latency() {
    const FRAMES: usize = 8192;

    let config = ElasticConfig::new(48_000, CHANNELS, FRAMES, FRAMES)
        .expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let latency = backend.capabilities().latency();
    let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
    let source = impulse_markers(FRAMES, 0);
    let mut output = vec![f32::NAN; FRAMES * CHANNELS];

    backend
        .process(request, &source, &mut output)
        .expect("unity is inside the supported envelope");

    assert_eq!(
        first_audible_frame(&output, CHANNELS),
        Some(latency.source_frames() + latency.output_frames())
    );
}

#[kithara::test]
fn warmup_request_has_exact_latency_spans() {
    let config =
        ElasticConfig::new(48_000, CHANNELS, 512, 512).expect("the test configuration is valid");
    let backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let latency = backend.capabilities().latency();

    let unity = backend
        .capabilities()
        .warmup_request(1.0)
        .expect("unity warmup request");
    assert_eq!(unity.source_frames(), latency.source_frames());
    assert_eq!(unity.output_frames(), latency.output_frames());

    let faster = backend
        .capabilities()
        .warmup_request(1.2)
        .expect("non-unity warmup request");
    assert_eq!(faster.source_frames(), 3456);
    assert_eq!(faster.output_frames(), latency.output_frames());

    let envelope = backend.capabilities().rate_envelope();
    let minimum = backend
        .capabilities()
        .warmup_request(envelope.min_source_frames_per_output())
        .expect("minimum-rate warmup request");
    assert_eq!(minimum.source_frames(), 1920);
    let maximum = backend
        .capabilities()
        .warmup_request(envelope.max_source_frames_per_output())
        .expect("maximum-rate warmup request");
    assert_eq!(maximum.source_frames(), 3840);
}

#[kithara::test]
fn history_and_output_warmup_remove_the_initial_gap() {
    const FRAMES: usize = 512;

    let config = ElasticConfig::new(48_000, CHANNELS, FRAMES * 2, FRAMES)
        .expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = backend.capabilities();
    let history_frames = capabilities.latency().source_frames();
    let history = impulse_markers(history_frames, 0);
    let warmup = capabilities
        .warmup_request(1.0)
        .expect("unity warmup request");
    let warm_source = impulse_markers(warmup.source_frames(), history_frames);
    let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    backend
        .prime(warmup, &history, &warm_source, &mut discarded)
        .expect("history and output latency warmup");
    let source = impulse_markers(FRAMES, history_frames + warmup.source_frames());
    let mut output = vec![0.0; FRAMES * CHANNELS];

    backend
        .process(
            ElasticRequest::new(FRAMES, FRAMES).expect("unity request"),
            &source,
            &mut output,
        )
        .expect("primed unity request");

    assert_eq!(first_audible_frame(&output, CHANNELS), Some(0));
}

#[kithara::test]
fn non_unity_warmup_aligns_the_first_audible_frame() {
    const SOURCE_FRAMES: usize = 600;
    const OUTPUT_FRAMES: usize = 500;

    let config = ElasticConfig::new(48_000, CHANNELS, SOURCE_FRAMES, OUTPUT_FRAMES)
        .expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = backend.capabilities();
    let history_frames = capabilities.latency().source_frames();
    let history = impulse_markers(history_frames, 0);
    let warmup = capabilities
        .warmup_request(1.2)
        .expect("non-unity warmup request");
    let warm_source = impulse_markers(warmup.source_frames(), history_frames);
    let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    backend
        .prime(warmup, &history, &warm_source, &mut discarded)
        .expect("non-unity history and output latency warmup");
    let source = impulse_markers(SOURCE_FRAMES, history_frames + warmup.source_frames());
    let mut output = vec![f32::NAN; OUTPUT_FRAMES * CHANNELS];

    backend
        .process(
            ElasticRequest::new(SOURCE_FRAMES, OUTPUT_FRAMES).expect("non-unity request"),
            &source,
            &mut output,
        )
        .expect("primed non-unity request");

    assert!(output.iter().all(|sample| sample.is_finite()));
    assert_eq!(first_audible_frame(&output, CHANNELS), Some(0));
}

#[kithara::test]
fn prime_rejects_every_ambiguous_buffer_count() {
    let config =
        ElasticConfig::new(48_000, CHANNELS, 1024, 512).expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = backend.capabilities();
    let warmup = capabilities
        .warmup_request(1.0)
        .expect("unity warmup request");
    let history = vec![0.25; capabilities.latency().source_frames() * CHANNELS];
    let source = vec![0.25; warmup.source_frames() * CHANNELS];
    let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];

    assert_eq!(
        backend.prime(
            warmup,
            &history[..history.len() - 1],
            &source,
            &mut discarded
        ),
        Err(ElasticError::HistorySampleCount {
            actual: history.len() - 1,
            expected: history.len(),
        })
    );
    assert_eq!(
        backend.prime(
            warmup,
            &history,
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
        backend.prime(
            warmup,
            &history,
            &source,
            &mut discarded[..discarded_len - 1],
        ),
        Err(ElasticError::OutputSampleCount {
            actual: discarded_len - 1,
            expected: discarded_len,
        })
    );
    let wrong_output = ElasticRequest::new(warmup.source_frames(), warmup.output_frames() - 1)
        .expect("non-empty mismatched warmup request");
    assert_eq!(
        backend.prime(wrong_output, &history, &source, &mut discarded),
        Err(ElasticError::WarmupOutputFrameCount {
            actual: warmup.output_frames() - 1,
            expected: warmup.output_frames(),
        })
    );
}

#[kithara::test]
fn keeps_the_same_latency_through_unity_and_rate_changes() {
    let config =
        ElasticConfig::new(48_000, CHANNELS, 8192, 8192).expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = backend.capabilities();
    assert_eq!(capabilities.sample_rate(), 48_000);
    assert_eq!(capabilities.channels(), CHANNELS);
    let envelope = capabilities.rate_envelope();
    assert_eq!(envelope.min_source_frames_per_output(), 2.0 / 3.0);
    assert_eq!(envelope.max_source_frames_per_output(), 4.0 / 3.0);
    let latency = capabilities.latency();
    assert_eq!(latency.source_frames(), 2880);
    assert_eq!(latency.output_frames(), 2880);

    for request in [
        ElasticRequest::new(4096, 4096).expect("unity request"),
        ElasticRequest::new(4800, 4000).expect("faster request"),
        ElasticRequest::new(4096, 4096).expect("unity request"),
    ] {
        let source = interleaved_signal(request.source_frames());
        let mut output = vec![f32::NAN; request.output_frames() * CHANNELS];
        backend
            .process(request, &source, &mut output)
            .expect("the request is supported");
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert_eq!(backend.capabilities().latency(), latency);
    }
}

#[kithara::test]
fn reset_clears_stream_history_without_changing_capabilities() {
    const LONG_FRAMES: usize = 8192;
    const SHORT_FRAMES: usize = 4096;

    let config = ElasticConfig::new(48_000, CHANNELS, LONG_FRAMES, LONG_FRAMES)
        .expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = backend.capabilities();
    let source = interleaved_signal(LONG_FRAMES);
    let mut output = vec![0.0; LONG_FRAMES * CHANNELS];
    backend
        .process(
            ElasticRequest::new(LONG_FRAMES, LONG_FRAMES).expect("unity request"),
            &source,
            &mut output,
        )
        .expect("the warm request is supported");
    assert!(output.iter().any(|sample| sample.abs() > f32::EPSILON));

    backend.reset();
    output[..SHORT_FRAMES * CHANNELS].fill(f32::NAN);
    backend
        .process(
            ElasticRequest::new(SHORT_FRAMES, SHORT_FRAMES).expect("unity request"),
            &source[..SHORT_FRAMES * CHANNELS],
            &mut output[..SHORT_FRAMES * CHANNELS],
        )
        .expect("the request after reset is supported");

    assert_eq!(backend.capabilities(), capabilities);
    assert!(
        output[..SHORT_FRAMES * CHANNELS]
            .iter()
            .all(|sample| sample.abs() <= f32::EPSILON)
    );
}

#[kithara::test]
fn reset_reprime_keeps_the_first_frame_aligned() {
    const SOURCE_FRAMES: usize = 600;
    const OUTPUT_FRAMES: usize = 500;

    let config = ElasticConfig::new(48_000, CHANNELS, SOURCE_FRAMES, OUTPUT_FRAMES)
        .expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = backend.capabilities();
    let warmup = capabilities
        .warmup_request(1.2)
        .expect("non-unity warmup request");
    let history = vec![0.25; capabilities.latency().source_frames() * CHANNELS];
    let warm_source = vec![0.25; warmup.source_frames() * CHANNELS];
    let source = vec![0.25; SOURCE_FRAMES * CHANNELS];
    let request = ElasticRequest::new(SOURCE_FRAMES, OUTPUT_FRAMES).expect("non-unity request");
    let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    let mut output = vec![0.0; OUTPUT_FRAMES * CHANNELS];

    for cycle in 0..8 {
        if cycle > 0 {
            backend.reset();
        }
        backend
            .prime(warmup, &history, &warm_source, &mut discarded)
            .expect("reset engine primes again");
        backend
            .process(request, &source, &mut output)
            .expect("request after reset is supported");

        assert_eq!(backend.capabilities(), capabilities);
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
fn prime_discards_previous_stream_state() {
    const FRAMES: usize = 4096;

    let config = ElasticConfig::new(48_000, CHANNELS, FRAMES, FRAMES)
        .expect("the test configuration is valid");
    let mut fresh = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let mut reused = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = fresh.capabilities();
    let warmup = capabilities
        .warmup_request(1.0)
        .expect("unity warmup request");
    let history = indexed_markers(capabilities.latency().source_frames(), 0);
    let warm_source = indexed_markers(warmup.source_frames(), history.len() / CHANNELS);
    let source = indexed_markers(FRAMES, (history.len() + warm_source.len()) / CHANNELS);
    let dirty_source = interleaved_signal(FRAMES);
    let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
    let mut dirty_output = vec![0.0; FRAMES * CHANNELS];
    reused
        .process(request, &dirty_source, &mut dirty_output)
        .expect("the dirtying request is supported");

    let mut fresh_discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    let mut reused_discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    fresh
        .prime(warmup, &history, &warm_source, &mut fresh_discarded)
        .expect("fresh engine primes");
    reused
        .prime(warmup, &history, &warm_source, &mut reused_discarded)
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

#[kithara::test]
fn primed_output_is_independent_of_unity_request_partitioning() {
    const FRAMES: usize = 4096;
    const PARTITION_FRAMES: usize = 512;

    let config = ElasticConfig::new(48_000, CHANNELS, FRAMES, FRAMES)
        .expect("the test configuration is valid");
    let mut whole = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let mut partitioned = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let capabilities = whole.capabilities();
    let history_frames = capabilities.latency().source_frames();
    let warmup = capabilities
        .warmup_request(1.0)
        .expect("unity warmup request");
    let history = impulse_markers(history_frames, 0);
    let warm_source = impulse_markers(warmup.source_frames(), history_frames);
    let source = impulse_markers(FRAMES, history_frames + warmup.source_frames());
    let mut whole_discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    let mut partitioned_discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    whole
        .prime(warmup, &history, &warm_source, &mut whole_discarded)
        .expect("whole engine primes");
    partitioned
        .prime(warmup, &history, &warm_source, &mut partitioned_discarded)
        .expect("partitioned engine primes");

    let mut whole_output = vec![0.0; FRAMES * CHANNELS];
    whole
        .process(
            ElasticRequest::new(FRAMES, FRAMES).expect("whole unity request"),
            &source,
            &mut whole_output,
        )
        .expect("whole engine renders");
    let mut partitioned_output = vec![0.0; FRAMES * CHANNELS];
    for (source, output) in source
        .chunks_exact(PARTITION_FRAMES * CHANNELS)
        .zip(partitioned_output.chunks_exact_mut(PARTITION_FRAMES * CHANNELS))
    {
        partitioned
            .process(
                ElasticRequest::new(PARTITION_FRAMES, PARTITION_FRAMES)
                    .expect("partition unity request"),
                source,
                output,
            )
            .expect("partitioned engine renders");
    }

    assert_exact_samples(&partitioned_discarded, &whole_discarded);
    assert_exact_samples(&partitioned_output, &whole_output);
    assert_eq!(first_audible_frame(&partitioned_output, CHANNELS), Some(0));
}

#[kithara::test]
fn preserves_tone_pitch_when_source_advance_changes() {
    const SAMPLE_RATE: u32 = 48_000;
    const SOURCE_FRAMES: usize = 19_200;
    const OUTPUT_FRAMES: usize = 16_000;

    let config = ElasticConfig::new(SAMPLE_RATE, 1, SOURCE_FRAMES, OUTPUT_FRAMES)
        .expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
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

    backend
        .process(request, &source, &mut output)
        .expect("the request is supported");

    let latency = backend.capabilities().latency().output_frames();
    let positive_crossings = output[latency..]
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    assert!(
        (110..=130).contains(&positive_crossings),
        "expected a pitch-locked 440 Hz tone, counted {positive_crossings} positive crossings"
    );
}

#[kithara::test]
fn rejects_requests_outside_the_prepared_contract() {
    let config =
        ElasticConfig::new(48_000, CHANNELS, 8192, 8192).expect("the test configuration is valid");
    let mut backend = SignalsmithBackend::prepare(config).expect("Signalsmith prepares");
    let request = ElasticRequest::new(6000, 4000).expect("non-empty request");
    let source = interleaved_signal(request.source_frames());
    let mut output = vec![0.0; request.output_frames() * CHANNELS];

    assert_eq!(
        backend.process(request, &source, &mut output),
        Err(ElasticError::RateOutsideEnvelope {
            source_frames: 6000,
            output_frames: 4000,
        })
    );

    let request = ElasticRequest::new(4800, 4000).expect("non-empty request");
    assert_eq!(
        backend.process(request, &source[..source.len() - 1], &mut output),
        Err(ElasticError::SourceSampleCount {
            actual: source.len() - 1,
            expected: 4800 * CHANNELS,
        })
    );
}
