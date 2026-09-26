use super::*;

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn renders_the_requested_output_frame_count(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    let mut engine = prepared_backend(backend, 8192, 8192);
    let request = ElasticRequest::new(4800, 4000).expect("the request is non-empty");
    let source = interleaved_signal(stretch_pcm, request.source_frames());
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
fn renders_exact_spans_at_both_declared_rate_edges(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    let mut engine = prepared_backend(backend, 8192, 4096);
    let capabilities = engine.capabilities();

    for request in edge_requests(capabilities) {
        let source = interleaved_signal(stretch_pcm, request.source_frames());
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
fn output_is_independent_of_request_partitioning(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    for (source_frames, output_frames, source_partition, output_partition) in [
        (16_384, 16_384, 512, 512),
        (8192, 10_240, 512, 640),
        (16_384, 8192, 1024, 512),
    ] {
        let mut whole = prepared_backend(backend, source_frames, output_frames);
        let mut partitioned = prepared_backend(backend, source_frames, output_frames);
        let source = impulse_markers(stretch_pcm, source_frames, 0);
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
fn keeps_capabilities_stable_through_rate_changes(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
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
        let source = continuous_tone(stretch_pcm, request.source_frames(), source_position);
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
fn pitch_control_is_independent_of_exact_frame_advance(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    let mut reference = prepared_backend(backend, 8192, 8192);
    let mut pitched = prepared_backend(backend, 8192, 8192);
    let request = ElasticRequest::new(4096, 4096).expect("unity request");
    let mut changed = false;

    pitched.set_pitch(1.25).expect("positive pitch scale");
    for block in 0..4 {
        let source = stretch_pcm.ramp
            [block * 4096 * CHANNELS..(block * 4096 + request.source_frames()) * CHANNELS]
            .to_vec();
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
fn terminal_flush_reaches_last_source_audio_at_each_rate(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const FRAMES: usize = 8192;
    for request in [
        ElasticRequest::new(FRAMES / 2, FRAMES).expect("half-speed request"),
        ElasticRequest::new(FRAMES, FRAMES).expect("unity request"),
        ElasticRequest::new(FRAMES, FRAMES / 2).expect("double-speed request"),
    ] {
        let mut engine = prepared_backend(backend, FRAMES, FRAMES);
        let capabilities = engine.capabilities();
        let marker_frames = rate_aware_terminal_source_frames(capabilities, request);
        let source =
            terminal_marker_signal_with_span(stretch_pcm, request.source_frames(), marker_frames);
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
            !terminal_pattern_is_valid(&terminal[..terminal.len() - CHANNELS], expected_drained,),
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
    stretch_pcm: &'static StretchPcm,
) {
    const OUTPUT_FRAMES: usize = 8_000;
    const MAX_SOURCE_FRAMES: usize = OUTPUT_FRAMES * 4;

    let mut engine = prepared_backend(backend, MAX_SOURCE_FRAMES, OUTPUT_FRAMES);
    let capabilities = engine.capabilities();
    let source_frames = source_frames_at(rate, OUTPUT_FRAMES, false);
    let request = ElasticRequest::new(source_frames, OUTPUT_FRAMES)
        .expect("the exact-rate request is non-empty");
    let marker_frames = rate_aware_terminal_source_frames(capabilities, request);
    let source = terminal_marker_signal_with_span(stretch_pcm, source_frames, marker_frames);
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
fn terminal_flush_reaches_each_new_rate_within_declared_latency(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
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
            let source = continuous_tone(stretch_pcm, initial.source_frames(), source_position);
            let mut output = vec![0.0; initial.output_frames() * CHANNELS];
            engine
                .process(initial, &source, &mut output)
                .expect("the initial rate reaches audible steady state");
            source_position += initial.source_frames();
        }

        let settled_output_frames = capabilities.latency().output_frames();
        let settled = edge_request(capabilities, settled_output_frames, settled_is_minimum);
        let settled_source = short_marker_signal(stretch_pcm, settled.source_frames());
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
    stretch_pcm: &'static StretchPcm,
) {
    const OUTPUT_FRAMES: usize = 8192;
    const MAX_SOURCE_FRAMES: usize = OUTPUT_FRAMES * 4;

    for (initial_is_minimum, settled_is_minimum) in [(true, false), (false, true)] {
        let mut engine = prepared_backend(backend, MAX_SOURCE_FRAMES, OUTPUT_FRAMES);
        let capabilities = engine.capabilities();
        let initial = edge_request(capabilities, OUTPUT_FRAMES, initial_is_minimum);
        let mut source_position = 0;
        for _ in 0..2 {
            let source = continuous_tone(stretch_pcm, initial.source_frames(), source_position);
            let mut output = vec![0.0; initial.output_frames() * CHANNELS];
            engine
                .process(initial, &source, &mut output)
                .expect("the practical initial edge reaches steady state");
            source_position += initial.source_frames();
        }

        let settled_output_frames = capabilities.latency().output_frames();
        let settled = edge_request(capabilities, settled_output_frames, settled_is_minimum);
        let source = continuous_tone(stretch_pcm, settled.source_frames(), source_position);
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
fn transitional_eof_preserves_every_indexed_marker(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
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
            let source = landmark_signal(stretch_pcm, initial.source_frames(), &landmarks);
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
        let transition = ElasticRequest::new(transition_source_frames, transition_output_frames)
            .expect("the short transition request is valid");
        let source = short_marker_signal(stretch_pcm, transition.source_frames());
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
fn flush_rejects_partial_frame_storage_without_disarming_tail(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const FRAMES: usize = 8192;

    let mut engine = prepared_backend(backend, FRAMES, FRAMES);
    let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
    let source = interleaved_signal(stretch_pcm, FRAMES);
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
fn reset_engine_has_no_terminal_tail(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const FRAMES: usize = 8192;

    let mut engine = prepared_backend(backend, FRAMES, FRAMES);
    let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
    let source = interleaved_signal(stretch_pcm, FRAMES);
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
fn reset_clears_stream_history_without_changing_capabilities(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const LONG_FRAMES: usize = 16_384;
    const SHORT_FRAMES: usize = 4096;

    let mut engine = prepared_backend(backend, LONG_FRAMES, LONG_FRAMES);
    let mut fresh = prepared_backend(backend, LONG_FRAMES, LONG_FRAMES);
    let capabilities = engine.capabilities();
    assert_eq!(fresh.capabilities(), capabilities);
    let source = interleaved_signal(stretch_pcm, LONG_FRAMES);
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
fn preserves_tone_pitch_when_source_advance_changes(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const SOURCE_FRAMES: usize = 19_200;
    const OUTPUT_FRAMES: usize = 16_000;

    let config = ElasticConfig::builder()
        .backend(backend)
        .backends(conformance_backends())
        .pools(default_pools())
        .sample_rate(SAMPLE_RATE)
        .channels(1)
        .max_source_frames(SOURCE_FRAMES)
        .max_output_frames(OUTPUT_FRAMES)
        .build()
        .expect("the test configuration is valid");
    let mut engine = build_engine(config).expect("the selected engine prepares");
    let request = ElasticRequest::new(SOURCE_FRAMES, OUTPUT_FRAMES).expect("non-empty request");
    let source = &stretch_pcm.mono[..SOURCE_FRAMES];
    let mut output = vec![0.0; OUTPUT_FRAMES];

    engine
        .process(request, source, &mut output)
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
fn rejects_buffers_that_do_not_match_the_request(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    let mut engine = prepared_backend(backend, 8192, 8192);
    let request = ElasticRequest::new(4800, 4000).expect("non-empty request");
    let source = interleaved_signal(stretch_pcm, request.source_frames());
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
fn rejects_spans_beyond_the_prepared_block_limits(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const MAX_SOURCE_FRAMES: usize = 2048;
    const MAX_OUTPUT_FRAMES: usize = 2048;

    let mut engine = prepared_backend(backend, MAX_SOURCE_FRAMES, MAX_OUTPUT_FRAMES);
    let mut output = vec![0.0; MAX_OUTPUT_FRAMES * CHANNELS];
    let source = interleaved_signal(stretch_pcm, MAX_SOURCE_FRAMES + 1);

    let request =
        ElasticRequest::new(MAX_SOURCE_FRAMES + 1, MAX_OUTPUT_FRAMES).expect("non-empty request");
    assert_eq!(
        engine.process(request, &source, &mut output),
        Err(ElasticError::SourceFrameLimit {
            frames: MAX_SOURCE_FRAMES + 1,
            limit: MAX_SOURCE_FRAMES,
        })
    );

    let mut long_output = vec![0.0; (MAX_OUTPUT_FRAMES + 1) * CHANNELS];
    let request =
        ElasticRequest::new(MAX_SOURCE_FRAMES, MAX_OUTPUT_FRAMES + 1).expect("non-empty request");
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
fn plans_and_renders_one_block_of_continuous_source_spans(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
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

    let source = interleaved_signal(stretch_pcm, OUTPUT_FRAMES);
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
