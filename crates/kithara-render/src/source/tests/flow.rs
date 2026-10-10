use super::*;

#[cfg(any(
    feature = "stretch-identity",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native)]
#[cfg_attr(
    feature = "stretch-identity",
    case::identity(StretchKind::Identity, false)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith, true)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee, true))]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide, true))]
fn upstream_terminal_failure_keeps_its_classification(
    #[case] backend: StretchKind,
    #[case] staged: bool,
) {
    let pools = pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
    let failure = TrackFailureKind::RecreateFailed { offset: 91 };
    let raw = FailedSource {
        failure,
        chunks: VecDeque::new(),
    };
    let config = kithara_warp::WarpConfig::builder()
        .backend(backend)
        .speed(0.5)
        .keylock(true)
        .build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let drain = EffectDrain::new(0, &pools).expect("empty effect drain");
    let mut source = WarpSource::new(
        raw,
        renderer,
        Vec::new(),
        drain,
        spec,
        pools.clone(),
        LaneSetup {
            inbox: idle_inbox(),
            preload_chunks: NonZeroUsize::MIN,
            declick: consts::DEFAULT_DECLICK,
        },
    );
    flush_deferred(&mut source);
    assert_eq!(source.warp.requires_staging(), staged);
    let step = source.step_track();
    assert!(matches!(step, TrackStep::Failed(actual) if actual == failure));
    assert!(!source.quantum_failed);
}

#[cfg(any(
    feature = "stretch-identity",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native)]
#[cfg_attr(
    feature = "stretch-identity",
    case::identity(StretchKind::Identity, false)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith, true)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee, true))]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide, true))]
fn upstream_terminal_failure_keeps_its_classification_after_buffered_pcm(
    #[case] backend: StretchKind,
    #[case] staged: bool,
    quarter: Vec<f32>,
) {
    let pools = pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
    let failure = TrackFailureKind::RecreateFailed { offset: 91 };
    let raw = FailedSource {
        failure,
        chunks: VecDeque::from([
            chunk_with_frames(&pools, spec, 0, 4096, &quarter),
            chunk_with_frames(&pools, spec, 4096, 17, &quarter),
        ]),
    };
    let config = kithara_warp::WarpConfig::builder()
        .backend(backend)
        .speed(0.5)
        .keylock(true)
        .render_quantum_frames(NonZeroUsize::new(128).expect("test quantum"))
        .build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let effects: Vec<Box<dyn AudioEffect>> = vec![Box::<BufferThenHalveFrames>::default()];
    let drain = EffectDrain::new(effects.len(), &pools).expect("buffered effect drain");
    let mut source = WarpSource::new(
        raw,
        renderer,
        effects,
        drain,
        spec,
        pools.clone(),
        LaneSetup {
            inbox: idle_inbox(),
            preload_chunks: NonZeroUsize::MIN,
            declick: consts::DEFAULT_DECLICK,
        },
    );
    let mut staged_prefix_seen = false;
    let mut produced_nonzero_pcm = false;
    let mut terminal_seen = false;
    for _ in 0..128 {
        flush_deferred(&mut source);
        assert_eq!(source.warp.requires_staging(), staged);
        let step = source.step_track();
        staged_prefix_seen |= source.pending_input.as_ref().is_some_and(|pending| {
            pending.consumed_frames > 0 && pending.consumed_frames < pending.chunk.frames()
        });
        match step {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                assert_eq!(data.meta.segment, SegmentId::FIRST);
                assert!(data.frames() > 0);
                assert_eq!(data.samples.len(), data.frames() * 2);
                assert!(data.samples.iter().all(|sample| sample.is_finite()));
                produced_nonzero_pcm |= data.samples.iter().any(|sample| *sample != 0.0);
            }
            TrackStep::StateChanged => {}
            TrackStep::Failed(actual) => {
                assert_eq!(actual, failure);
                assert!(source.source.chunks.is_empty());
                assert!(held_source_frames(&source.effects) > 0);
                assert!(matches!(source.drain_state, DrainState::Open));
                assert!(!source.quantum_failed);
                terminal_seen = true;
                break;
            }
            _ => panic!("buffered PCM must produce or preserve the upstream failure"),
        }
    }
    assert!(
        produced_nonzero_pcm,
        "the configured backend must render PCM before failure"
    );
    assert!(terminal_seen, "the upstream failure must remain terminal");
    assert_eq!(staged_prefix_seen, staged);
    for _ in 0..3 {
        flush_deferred(&mut source);
        assert!(matches!(source.step_track(), TrackStep::Failed(actual) if actual == failure));
        assert!(held_source_frames(&source.effects) > 0);
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native)]
#[case::q16(16)]
#[case::q32(32)]
fn unity_source_chunks_bypass_staging_without_losing_terminal_input(
    #[case] quantum_frames: usize,
    quarter: Vec<f32>,
    negative_half: Vec<f32>,
    three_quarter: Vec<f32>,
) {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
    let pools = pools();
    let input_frames = [20_usize, 20, 10];
    let chunks = [
        chunk_with_frames(&pools, spec, 0, 20, &quarter),
        chunk_with_frames(&pools, spec, 20, 20, &negative_half),
        chunk_with_frames(&pools, spec, 40, 10, &three_quarter),
    ];
    let expected_samples = chunks
        .iter()
        .flat_map(|chunk| chunk.samples.iter().copied())
        .collect::<Vec<_>>();
    let source = RawSource {
        chunks: VecDeque::from(chunks),
        head: Arc::new(AtomicU64::new(0)),
    };
    let mut source = source_stage_with_quantum(&pools, source, Vec::new(), spec, quantum_frames);
    let mut output_frames = Vec::new();
    let mut output_samples = Vec::new();

    for _ in 0..32 {
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                output_frames.push(data.frames());
                output_samples.extend_from_slice(&data.samples);
            }
            TrackStep::StateChanged => {}
            TrackStep::Eof => break,
            _ => panic!("staged source must only produce, progress, or finish"),
        }
        flush_deferred(&mut source);
    }

    assert!(output_frames.iter().all(|frames| *frames <= quantum_frames));
    assert_eq!(
        output_frames.iter().sum::<usize>(),
        input_frames.iter().copied().sum::<usize>()
    );
    assert_eq!(output_samples, expected_samples);
}

#[kithara::test]
fn buffered_frame_changing_effect_tracks_live_and_flush_frontiers(quarter: Vec<f32>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
    let pools = pools();
    let head = Arc::new(AtomicU64::new(0));
    let source = RawSource {
        chunks: VecDeque::from([
            chunk(&pools, spec, 0, &quarter),
            chunk(&pools, spec, u64::from(128_u32), &quarter),
        ]),
        head: Arc::clone(&head),
    };
    let effects: Vec<Box<dyn AudioEffect>> = vec![Box::<BufferThenHalveFrames>::default()];
    let mut source = source_stage(&pools, source, effects, spec);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert_eq!(
        head.load(Ordering::Acquire),
        128,
        "one worker pass advances exactly one source transition"
    );
    flush_deferred(&mut source);
    let mut produced = None;
    for _ in 0..3 {
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                produced = Some(fetch);
                break;
            }
            TrackStep::StateChanged => flush_deferred(&mut source),
            _ => panic!("the second raw chunk must release the first buffered span"),
        }
    }
    let Some(Fetch::Data {
        data, source_end, ..
    }) = produced
    else {
        panic!("the second raw chunk must release the first buffered span");
    };

    assert_eq!(head.load(Ordering::Acquire), 256);
    assert_eq!(data.meta.frame_offset, 0);
    assert_eq!(data.meta.frames, 64);
    assert_eq!(
        source_end,
        Some(SourceEnd::new(
            128,
            NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        )),
        "the buffered second span remains outside the live source frontier"
    );

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    let TrackStep::Produced(Fetch::Data {
        data, source_end, ..
    }) = source.step_track()
    else {
        panic!("EOF drain must release the second buffered span");
    };

    assert_eq!(data.meta.frame_offset, 128);
    assert_eq!(data.meta.frames, 64);
    assert_eq!(
        source_end,
        Some(SourceEnd::new(
            256,
            NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        )),
        "terminal output releases the held source frontier"
    );
}

#[kithara::test]
fn deferred_shell_services_effects_between_source_phases() {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
    let pools = pools();
    let log = Arc::new(Mutex::new(Vec::new()));
    let serviced = Arc::new(Mutex::new(None));
    let source = DeferredSource {
        spec,
        log: Arc::clone(&log),
    };
    let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(DeferredEffect {
        log: Arc::clone(&log),
        serviced: Arc::clone(&serviced),
    })];
    let mut source = source_stage(&pools, source, effects, spec);

    flush_deferred(&mut source);

    assert_eq!(
        log.lock().as_slice(),
        ["source.prepare", "effect.service", "source.finish"]
    );
    assert_eq!(*serviced.lock(), Some(spec));
}

#[kithara::test]
fn discontinuity_refreshes_spec_without_resetting_same_revision() {
    let initial = AudioSpec::new(2, NonZeroU32::new(44_100).expect("initial rate"));
    let changed = AudioSpec::new(1, NonZeroU32::new(48_000).expect("changed rate"));
    let pools = pools();
    let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(7, initial)));
    let resets = Arc::new(AtomicU64::new(0));
    let source = RevisionSource {
        chunks: VecDeque::new(),
        discontinuity: Arc::clone(&discontinuity),
    };
    let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(ResetCounter {
        resets: Arc::clone(&resets),
    })];
    let mut source = source_stage(&pools, source, effects, initial);

    *discontinuity.lock() = SourceDiscontinuity::new(7, changed);
    flush_deferred(&mut source);
    assert_eq!(
        source.discontinuity().map(|stamp| *stamp.spec()),
        Some(changed)
    );
    assert_eq!(resets.load(Ordering::Acquire), 0);

    *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
    flush_deferred(&mut source);
    assert_eq!(resets.load(Ordering::Acquire), 1);
}

#[kithara::test]
fn unity_warp_preserves_samples_and_meta_across_discontinuity(
    quarter: Vec<f32>,
    negative_quarter: Vec<f32>,
) {
    let initial = AudioSpec::new(2, NonZeroU32::new(44_100).expect("initial rate"));
    let changed = AudioSpec::new(1, NonZeroU32::new(48_000).expect("changed rate"));
    let pools = pools();
    let first = chunk(&pools, initial, 256, &quarter);
    let mut first_meta = first.meta;
    first_meta.source_span = SourceSpan::new(256, 384, initial.sample_rate, 128);
    let first_samples = first.samples.to_vec();
    let mut second = chunk(&pools, changed, 512, &negative_quarter);
    second.meta.segment_index = Some(3);
    second.meta.variant_index = Some(2);
    second.meta.segment = SegmentId::FIRST.next();
    second.meta.source_byte_offset = Some(4096);
    second.meta.source_bytes = 1024;
    let mut second_meta = second.meta;
    second_meta.segment = SegmentId::FIRST;
    second_meta.lane_frame = 128;
    second_meta.source_span = SourceSpan::new(512, 640, changed.sample_rate, 128);
    let second_samples = second.samples.to_vec();
    let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(7, initial)));
    let source = RevisionSource {
        chunks: VecDeque::from([first, second]),
        discontinuity: Arc::clone(&discontinuity),
    };
    let effects = Vec::new();
    let mut source = source_stage(&pools, source, effects, initial);

    let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
        panic!("initial unity span must pass through");
    };
    assert_eq!(data.meta, first_meta);
    assert_eq!(&data.samples[..], &first_samples);

    *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
    flush_deferred(&mut source);
    let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
        panic!("post-discontinuity unity span must pass through");
    };
    assert_eq!(data.meta, second_meta);
    assert_eq!(&data.samples[..], &second_samples);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn live_warp_drain_holds_the_source_and_seek_discards_stale_unity(
    half: Vec<f32>,
    quarter: Vec<f32>,
    three_quarter: Vec<f32>,
) {
    fn feed_whole_chunk(source: &mut WarpSource<RawSource, TestPools>) -> TrackStep<AudioChunk> {
        let TrackStep::Produced(Fetch::Data { data, .. }) = source.source.step_track() else {
            panic!("fixture provides a whole decoded chunk");
        };
        source.warp.prepare(source.spec);
        let output = source
            .warp
            .render(data)
            .continue_value()
            .expect("whole source span");
        if source.warp.transition_pending() {
            source.drain_state = DrainState::LiveWarp;
        }
        output
            .and_then(|data| source.emit_output(data, None))
            .map_or(TrackStep::StateChanged, TrackStep::Produced)
    }

    for backend in keylock_backends() {
        const ACTIVE_FRAMES: u32 = 4096;
        const UNITY_FRAMES: u32 = 4096;
        const SENTINEL_FRAMES: u32 = 4096;

        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let first_active_end = u64::from(ACTIVE_FRAMES);
        let first_unity_end = first_active_end.saturating_add(u64::from(UNITY_FRAMES));
        let sentinel_end = first_unity_end.saturating_add(u64::from(SENTINEL_FRAMES));

        let first_active = chunk_with_frames(&pools, spec, 0, ACTIVE_FRAMES, &quarter);
        let first_unity = chunk_with_frames(&pools, spec, first_active_end, UNITY_FRAMES, &half);
        let sentinel = chunk_with_frames(
            &pools,
            spec,
            first_unity_end,
            SENTINEL_FRAMES,
            &three_quarter,
        );
        let sentinel_ptr = sentinel.samples.as_ptr();
        let sentinel_samples = sentinel.samples.to_vec();

        let head = Arc::new(AtomicU64::new(0));
        let raw = RawSource {
            chunks: VecDeque::from([first_active, first_unity, sentinel]),
            head: Arc::clone(&head),
        };
        let render_quantum_frames = usize::try_from(ACTIVE_FRAMES)
            .expect("test quantum fits usize")
            .saturating_mul(2)
            .saturating_add(1);
        let config = kithara_warp::WarpConfig::builder()
            .speed(0.5)
            .keylock(true)
            .backend(backend)
            .render_quantum_frames(
                NonZeroUsize::new(render_quantum_frames).expect("test quantum is non-zero"),
            )
            .build();
        let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
        let effects = Vec::new();
        let drain = EffectDrain::new(effects.len(), &pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        let (mut lane, inbox) = channel::<LaneProtocol>(ChannelConfig::builder().build());
        let mut source = WarpSource::new(
            raw,
            renderer,
            effects,
            drain,
            spec,
            pools.clone(),
            LaneSetup {
                inbox,
                preload_chunks: NonZeroUsize::new(1).expect("preload"),
                declick: consts::DEFAULT_DECLICK,
            },
        );

        let initial = feed_whole_chunk(&mut source);
        assert!(matches!(
            &initial,
            TrackStep::Produced(_) | TrackStep::StateChanged
        ));
        assert_eq!(head.load(Ordering::Acquire), first_active_end);
        flush_deferred(&mut source);
        if matches!(&initial, TrackStep::StateChanged) {
            let TrackStep::Produced(_) = source.step_track() else {
                panic!("the first active quantum must render");
            };
            flush_deferred(&mut source);
        }

        source
            .warp
            .set_speed(SpeedCurve::Constant(1.0), 1)
            .expect("unity command");
        let transition = feed_whole_chunk(&mut source);
        assert!(matches!(
            &transition,
            TrackStep::Produced(_) | TrackStep::StateChanged
        ));
        assert_eq!(head.load(Ordering::Acquire), first_unity_end);
        flush_deferred(&mut source);
        if matches!(&transition, TrackStep::StateChanged) {
            let TrackStep::Produced(_) = source.step_track() else {
                panic!("active-to-unity transition must emit its first tail quantum");
            };
        }
        assert_eq!(head.load(Ordering::Acquire), first_unity_end);
        assert!(source.warp.transition_pending());

        lane.send(
            When::Next,
            command_batch(LaneCommand::Segment {
                id: SegmentId::FIRST.next(),
                from: Duration::from_secs(1),
                speed: SpeedCurve::Constant(1.0),
            }),
        )
        .expect("segment batch");
        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(source.cursor().segment, SegmentId::FIRST.next());
        assert_eq!(head.load(Ordering::Acquire), first_unity_end);
        assert!(!source.warp.transition_pending());

        flush_deferred(&mut source);
        let resumed = source.step_track();
        let resumed = match resumed {
            TrackStep::Produced(fetch) => fetch,
            TrackStep::StateChanged => {
                flush_deferred(&mut source);
                let TrackStep::Produced(fetch) = source.step_track() else {
                    panic!("playback must resume with the post-seek source chunk");
                };
                fetch
            }
            _ => panic!("playback must resume with the post-seek source chunk"),
        };
        let Fetch::Data { data, .. } = resumed else {
            panic!("playback must resume with post-seek audio data");
        };
        assert_eq!(head.load(Ordering::Acquire), sentinel_end);
        assert_eq!(data.samples.as_ptr(), sentinel_ptr);
        assert_eq!(&data.samples[..], &sentinel_samples);
        assert_eq!(data.meta.segment, SegmentId::FIRST.next());
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native)]
async fn rejected_staged_quantum_retains_both_owning_buffers(quarter: Vec<f32>) {
    const FRAMES: usize = 64;
    #[kithara::allow_block]
    fn prepare(quarter: &[f32]) -> (WarpSource<RawSource, TestPools>, *const f32, *const f32) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("sample rate"));
        let raw = RawSource {
            head: Arc::new(AtomicU64::new(0)),
            chunks: VecDeque::new(),
        };
        let decoded = chunk_with_frames(&pools, spec, 0, 64, quarter);
        let decoded_pointer = decoded.samples.as_ptr();
        let mut source = source_stage_with_quantum(&pools, raw, Vec::new(), spec, FRAMES);
        source.pending_input = Some(PendingInput {
            chunk: decoded,
            consumed_frames: 0,
        });
        source.prepare_staging();
        assert_eq!(source.prepared_frames, Some(FRAMES));
        assert!(source.stage_pending());
        assert!(source.pending_input.is_none());
        let staged_pointer = source.render_input.as_ref().expect("staged PCM").as_ptr();
        assert_ne!(decoded_pointer, staged_pointer);
        source.warp.reset();
        (source, decoded_pointer, staged_pointer)
    }
    #[kithara::no_block(budget_ms = 1_000)]
    async fn reject(source: &mut WarpSource<RawSource, TestPools>) -> TrackStep<AudioChunk> {
        source.render_staged(FRAMES)
    }
    #[kithara::allow_block]
    fn release(source: WarpSource<RawSource, TestPools>) {
        drop(source);
    }
    let (mut source, decoded_pointer, staged_pointer) = prepare(&quarter);
    let result = reject(&mut source).await;
    assert!(matches!(result, TrackStep::Failed(_)));
    assert!(source.quantum_failed);
    let decoded = source
        .retired_input
        .as_ref()
        .expect("decoded retirement retained");
    assert_eq!(decoded.samples.as_ptr(), decoded_pointer);
    assert_eq!(&decoded.samples[..], &quarter[..FRAMES * 2]);
    let staged = source
        .render_input
        .as_ref()
        .expect("rejected staging retained");
    assert_eq!(staged.as_ptr(), staged_pointer);
    assert_eq!(&staged[..], &quarter[..FRAMES * 2]);
    release(source);
}

#[kithara::test(native)]
#[cfg(feature = "stretch-signalsmith")]
fn projected_backend_switch_keeps_pending_input_during_resident_output(quarter: Vec<f32>) {
    use kithara_warp::{GridSegment, RegionPlan};
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("sample rate"));
    let pools = pools();
    let config = kithara_warp::WarpConfig::builder()
        .speed(1.0)
        .keylock(true)
        .backend(StretchKind::Signalsmith)
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .region_plan(Arc::new(
            RegionPlan::new(vec![GridSegment::new(0, 480_000, 1.5)]).expect("region geometry"),
        ))
        .build();
    let head = Arc::new(AtomicU64::new(0));
    let raw = RawSource {
        head: Arc::clone(&head),
        chunks: (0..8)
            .map(|index| chunk_with_frames(&pools, spec, index * 4096, 4096, &quarter))
            .collect(),
    };
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let drain = EffectDrain::new(0, &pools).expect("empty effect drain");
    let mut source = WarpSource::new(
        raw,
        renderer,
        Vec::new(),
        drain,
        spec,
        pools.clone(),
        LaneSetup {
            inbox: idle_inbox(),
            preload_chunks: NonZeroUsize::new(1).expect("preload"),
            declick: consts::DEFAULT_DECLICK,
        },
    );
    let mut produced = 0;
    for _ in 0..128 {
        flush_deferred(&mut source);
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                assert!(data.frames() > 0);
                produced += 1;
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            _ => panic!("projected source must stay live before the switch"),
        }
        if produced >= 4 && source.pending_input.is_some() && source.prepared_frames.is_none() {
            break;
        }
    }
    assert!(
        produced >= 4,
        "the old backend must emit before it is replaced"
    );
    let pending = source.pending_input.as_ref().expect("held decoded input");
    let pointer = pending.chunk.samples.as_ptr();
    let meta = pending.chunk.meta;
    let consumed = pending.consumed_frames;
    let input_head = head.load(Ordering::Acquire);
    source.warp.set_keylock(false);
    source.warp.prepare(spec);
    assert!(
        source.warp.transition_pending(),
        "the old backend needs retirement"
    );
    source.prepare_staging();
    assert!(
        !source.quantum_failed,
        "NeedsService is not a fatal render error"
    );
    assert!(
        matches!(source.drain_state, DrainState::LiveWarp),
        "backend retirement must be serviced before retrying the held input"
    );
    let mut resident_output = false;
    let mut resumed_input = false;
    for _ in 0..128 {
        flush_deferred(&mut source);
        let before = source
            .pending_input
            .as_ref()
            .expect("input remains held until resumed")
            .consumed_frames;
        let step = source.step_track();
        assert!(
            !matches!(step, TrackStep::Failed(_) | TrackStep::Eof),
            "backend service must neither fail nor finish the source"
        );
        let pending = source
            .pending_input
            .as_ref()
            .expect("one resumed quantum leaves decoded input");
        assert_eq!(pending.chunk.samples.as_ptr(), pointer);
        assert_eq!(pending.chunk.meta, meta);
        assert_eq!(
            &pending.chunk.samples[..],
            &quarter[..pending.chunk.samples.len()]
        );
        assert_eq!(head.load(Ordering::Acquire), input_head);
        if let TrackStep::Produced(Fetch::Data { data, .. }) = step {
            assert!(data.frames() > 0);
            if pending.consumed_frames == before {
                resident_output = true;
                assert_eq!(pending.consumed_frames, consumed);
            }
        }
        if pending.consumed_frames > consumed {
            resumed_input = true;
            break;
        }
    }
    assert!(
        resident_output,
        "buffered projection must emit without taking new decoded frames"
    );
    assert!(
        resumed_input,
        "the replacement backend must resume the retained input"
    );
    assert!(!source.quantum_failed);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn unavailable_warp_target_fails_before_pulling_source(quarter: Vec<f32>) {
    for backend in StretchKind::all()
        .iter()
        .copied()
        .filter(|backend| backend.capabilities().contains(WarpCapabilities::RATE))
    {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let source_pools = pools();
        let head = Arc::new(AtomicU64::new(0));
        let raw = RawSource {
            chunks: VecDeque::from([chunk(&source_pools, spec, 0, &quarter)]),
            head: Arc::clone(&head),
        };
        let config = kithara_warp::WarpConfig::builder()
            .speed(0.5)
            .keylock(true)
            .backend(backend)
            .build();
        let target_pools = pools_with_budget(0);
        let renderer = kithara_warp::Warp::new((), &config).renderer(spec, target_pools.clone());
        let effects = Vec::new();
        let drain = EffectDrain::new(effects.len(), &target_pools)
            .unwrap_or_else(|error| panic!("test effect drain: {error}"));
        let mut source = WarpSource::new(
            raw,
            renderer,
            effects,
            drain,
            spec,
            target_pools.clone(),
            LaneSetup {
                inbox: idle_inbox(),
                preload_chunks: NonZeroUsize::new(1).expect("preload"),
                declick: consts::DEFAULT_DECLICK,
            },
        );

        for _ in 0..3 {
            flush_deferred(&mut source);
            assert!(matches!(source.step_track(), TrackStep::Failed(_)));
            assert_eq!(head.load(Ordering::Acquire), 0);
        }
    }
}

#[kithara::test]
fn seek_cancels_stale_tail_and_resets_effects_once(quarter: Vec<f32>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
    let pools = pools();
    let resets = Arc::new(AtomicU64::new(0));
    let source = SeekApplyingSource {
        spec,
        revision: 0,
        pending: false,
    };
    let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(ResettingTail {
        resets: Arc::clone(&resets),
        tail: Some(chunk(&pools, spec, 128, &quarter)),
    })];
    let mut source = source_stage(&pools, source, effects, spec);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(
        source.source.seek(Duration::from_secs(1)),
        Ok(SeekOutcome::Landed { .. })
    ));
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert_eq!(
        resets.load(Ordering::Acquire),
        1,
        "stale seek drain resets renderers before the source adopts the epoch"
    );
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert_eq!(resets.load(Ordering::Acquire), 1);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::Eof));
}

#[kithara::test]
fn every_effect_tail_precedes_the_single_eof(quarter: Vec<f32>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
    let pools = pools();
    let source = RawSource {
        chunks: VecDeque::new(),
        head: Arc::new(AtomicU64::new(0)),
    };
    let effects: Vec<Box<dyn AudioEffect>> = vec![
        Box::new(ResettingTail {
            resets: Arc::new(AtomicU64::new(0)),
            tail: Some(chunk(&pools, spec, 128, &quarter)),
        }),
        Box::new(ResettingTail {
            resets: Arc::new(AtomicU64::new(0)),
            tail: Some(chunk(&pools, spec, 256, &quarter)),
        }),
    ];
    let mut source = source_stage(&pools, source, effects, spec);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    for expected in [128, 256] {
        let TrackStep::Produced(Fetch::Data {
            data, source_end, ..
        }) = source.step_track()
        else {
            panic!("effect tail must be emitted before EOF");
        };
        assert_eq!(data.meta.frame_offset, expected);
        assert_eq!(source_end, None, "effect-only tails do not advance source");
    }
    assert!(matches!(source.step_track(), TrackStep::Eof));
}
