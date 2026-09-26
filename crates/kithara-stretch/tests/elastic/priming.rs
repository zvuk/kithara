use super::*;

type PrimedPair = (
    Box<dyn ElasticEngine>,
    Box<dyn ElasticEngine>,
    ElasticCapabilities,
    usize,
);

fn indexed_markers(pcm: &StretchPcm, frames: usize, offset: usize) -> Vec<f32> {
    pcm.indexed[offset * CHANNELS..(offset + frames) * CHANNELS].to_vec()
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
    ElasticRequest::new(source_frames, output_frames).expect("invariant: warmup request is valid")
}

fn primed_playing_pair(stretch_pcm: &StretchPcm, backend: StretchKind) -> PrimedPair {
    const MAX_FRAMES: usize = 65_536;

    let mut reference = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
    let mut changed = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
    let capabilities = reference.capabilities();
    assert_eq!(changed.capabilities(), capabilities);
    let latency = capabilities.latency();
    let warmup = warmup_request(capabilities, 1.0);
    let history = indexed_markers(stretch_pcm, latency.source_frames(), 0);
    let lookahead = indexed_markers(
        stretch_pcm,
        latency.source_frames(),
        latency.source_frames(),
    );
    let warm_source = indexed_markers(
        stretch_pcm,
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
    let source = indexed_markers(stretch_pcm, CONTROL_QUANTUM, continuation);
    let request =
        ElasticRequest::new(CONTROL_QUANTUM, CONTROL_QUANTUM).expect("lead quantum is non-empty");
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
    stretch_pcm: &StretchPcm,
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
        let reference_source = indexed_markers(stretch_pcm, output_frames, reference_position);
        let changed_source = indexed_markers(stretch_pcm, changed_source_frames, changed_position);
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
fn history_and_output_warmup_remove_the_initial_gap(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const FRAMES: usize = 512;

    let mut engine = prepared_backend(backend, FRAMES * 2, FRAMES);
    let capabilities = engine.capabilities();
    let history_frames = capabilities.latency().source_frames();
    let history = stretch_pcm.quarter[..history_frames * CHANNELS].to_vec();
    let lookahead = stretch_pcm.quarter[..history.len()].to_vec();
    let warmup = warmup_request(capabilities, 1.0);
    let warm_source = stretch_pcm.quarter[..warmup.source_frames() * CHANNELS].to_vec();
    let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    engine
        .prime(warmup, &history, &lookahead, &warm_source, &mut discarded)
        .expect("history and output latency warmup");
    let source = stretch_pcm.quarter[..FRAMES * CHANNELS].to_vec();
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
fn post_prime_pitch_change_responds_within_declared_latency(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    let (mut reference, mut changed, capabilities, continuation) =
        primed_playing_pair(stretch_pcm, backend);

    assert_control_response(
        stretch_pcm,
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
fn post_prime_rate_change_responds_within_declared_latency(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    let (mut reference, mut changed, capabilities, continuation) =
        primed_playing_pair(stretch_pcm, backend);

    assert_control_response(
        stretch_pcm,
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
    stretch_pcm: &'static StretchPcm,
) {
    const MAX_FRAMES: usize = 65_536;
    const TRANSITIONS: usize = 32;

    let mut engine = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
    let capabilities = engine.capabilities();
    let latency = capabilities.latency();
    let warmup = warmup_request(capabilities, 1.0);
    let history = continuous_tone(stretch_pcm, latency.source_frames(), 0);
    let lookahead = continuous_tone(
        stretch_pcm,
        latency.source_frames(),
        latency.source_frames(),
    );
    let warm_offset = latency.source_frames().saturating_mul(2);
    let warm_source = continuous_tone(stretch_pcm, warmup.source_frames(), warm_offset);
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
        let source = continuous_tone(stretch_pcm, source_frames, source_position);
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
fn sustained_pitch_alternation_renders_only_admitted_source(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const MAX_FRAMES: usize = 65_536;
    const TRANSITIONS: usize = 1024;

    let mut engine = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
    let latency = engine.capabilities().latency();
    let warmup = warmup_request(engine.capabilities(), 1.0);
    let history = continuous_tone(stretch_pcm, latency.source_frames(), 0);
    let lookahead = continuous_tone(
        stretch_pcm,
        latency.source_frames(),
        latency.source_frames(),
    );
    let warm_offset = latency.source_frames().saturating_mul(2);
    let warm_source = continuous_tone(stretch_pcm, warmup.source_frames(), warm_offset);
    let mut discarded = vec![0.0; warmup.output_frames() * CHANNELS];
    engine
        .prime(warmup, &history, &lookahead, &warm_source, &mut discarded)
        .expect("the continuous fixture primes at unity");

    let request =
        ElasticRequest::new(CONTROL_QUANTUM, CONTROL_QUANTUM).expect("unity quantum is non-empty");
    let mut source_position = warm_offset.saturating_add(warmup.source_frames());
    for transition in 0..TRANSITIONS {
        let pitch = if transition.is_multiple_of(2) {
            2.0
        } else {
            1.0
        };
        engine
            .set_pitch(pitch)
            .expect("the alternating pitch stays inside the common range");
        let source = continuous_tone(stretch_pcm, CONTROL_QUANTUM, source_position);
        let mut output = vec![f32::NAN; CONTROL_QUANTUM * CHANNELS];
        if let Err(error) = engine.process(request, &source, &mut output) {
            panic!("transition {transition} of {backend:?} left the admitted source: {error}");
        }
        assert!(
            output.iter().all(|sample| sample.is_finite()),
            "transition {transition} produced a non-finite sample"
        );
        source_position = source_position.saturating_add(CONTROL_QUANTUM);
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn source_history_conditions_the_cue_boundary(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const MAX_FRAMES: usize = 65_536;

    let mut conditioned = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
    let mut zero_padded = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
    let capabilities = conditioned.capabilities();
    assert_eq!(zero_padded.capabilities(), capabilities);
    let latency = capabilities.latency();
    let warmup = warmup_request(capabilities, 1.0);
    let history = continuous_tone(stretch_pcm, latency.source_frames(), 0);
    let empty_history = stretch_pcm.silence[..history.len()].to_vec();
    let lookahead = continuous_tone(
        stretch_pcm,
        latency.source_frames(),
        latency.source_frames(),
    );
    let warm_source = continuous_tone(
        stretch_pcm,
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
        stretch_pcm,
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
fn prime_accepts_declared_rate_edges(
    #[case] backend: StretchKind,
    #[case] rate: f64,
    stretch_pcm: &'static StretchPcm,
) {
    const FRAMES: usize = 512;

    let mut engine = prepared_backend(backend, FRAMES * 2, FRAMES);
    let capabilities = engine.capabilities();
    let history_frames = capabilities.latency().source_frames();
    let output_frames = capabilities.latency().output_frames();
    let source_frames = source_frames_at(rate, output_frames, rate < 1.0);
    let request = ElasticRequest::new(source_frames, output_frames)
        .expect("the declared edge request is non-empty");
    let history = stretch_pcm.quarter[..history_frames * CHANNELS].to_vec();
    let lookahead = stretch_pcm.quarter[..history.len()].to_vec();
    let source = stretch_pcm.quarter[..source_frames * CHANNELS].to_vec();
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
    stretch_pcm: &'static StretchPcm,
) {
    const MAX_FRAMES: usize = 65_536;
    const FOLLOWING_FRAMES: usize = 4096;

    let mut engine = prepared_backend(backend, MAX_FRAMES, MAX_FRAMES);
    let capabilities = engine.capabilities();
    let history_frames = capabilities.latency().source_frames();
    let warmup = warmup_request(capabilities, source_frames_per_output);
    let history = stretch_pcm.nine[..history_frames * CHANNELS].to_vec();
    let lookahead = stretch_pcm.fifth[..history.len()].to_vec();
    let warm_source = stretch_pcm.half[..warmup.source_frames() * CHANNELS].to_vec();
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
    let source = stretch_pcm.four_fifths[..render_source_frames * CHANNELS].to_vec();
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
fn prime_rejects_every_ambiguous_buffer_count(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    let mut engine = prepared_backend(backend, 1024, 512);
    let capabilities = engine.capabilities();
    let warmup = warmup_request(capabilities, 1.0);
    let history = stretch_pcm.quarter[..capabilities.latency().source_frames() * CHANNELS].to_vec();
    let lookahead = stretch_pcm.quarter[..history.len()].to_vec();
    let source = stretch_pcm.quarter[..warmup.source_frames() * CHANNELS].to_vec();
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
fn reset_reprime_keeps_the_first_frame_aligned(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const SOURCE_FRAMES: usize = 600;
    const OUTPUT_FRAMES: usize = 500;

    let mut engine = prepared_backend(backend, SOURCE_FRAMES, OUTPUT_FRAMES);
    let capabilities = engine.capabilities();
    let warmup = warmup_request(capabilities, 1.2);
    let history = stretch_pcm.quarter[..capabilities.latency().source_frames() * CHANNELS].to_vec();
    let lookahead = stretch_pcm.quarter[..history.len()].to_vec();
    let warm_source = stretch_pcm.quarter[..warmup.source_frames() * CHANNELS].to_vec();
    let source = stretch_pcm.quarter[..SOURCE_FRAMES * CHANNELS].to_vec();
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
fn prime_discards_previous_stream_state(
    #[case] backend: StretchKind,
    stretch_pcm: &'static StretchPcm,
) {
    const FRAMES: usize = 4096;

    let mut fresh = prepared_backend(backend, FRAMES, FRAMES);
    let mut reused = prepared_backend(backend, FRAMES, FRAMES);
    let capabilities = fresh.capabilities();
    let warmup = warmup_request(capabilities, 1.0);
    let history_frames = capabilities.latency().source_frames();
    let history = indexed_markers(stretch_pcm, history_frames, 0);
    let lookahead = indexed_markers(stretch_pcm, history_frames, history_frames);
    let warm_source = indexed_markers(stretch_pcm, warmup.source_frames(), history_frames * 2);
    let source = indexed_markers(
        stretch_pcm,
        FRAMES,
        history_frames * 2 + warmup.source_frames(),
    );
    let dirty_source = interleaved_signal(stretch_pcm, FRAMES);
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
