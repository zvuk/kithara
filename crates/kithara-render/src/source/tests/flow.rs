use kithara_signal::AudioChunkInfo;
use kithara_test_utils::bufpool::sample_buffer;
use kithara_warp::Pitch;

use super::*;

#[kithara::test]
#[case::zero_channels(0, false)]
#[case::zero_channels_final(0, true)]
#[case::incomplete_stereo(2, false)]
#[case::incomplete_stereo_final(2, true)]
fn malformed_pcm_is_a_terminal_render_failure_before_it_enters_the_deck(
    #[case] channels: u16,
    #[case] end_of_track: bool,
) {
    let pools = pools();
    let rate = NonZeroU32::new(44_100).expect("rate");
    let invalid = AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec::new(channels, rate),
            frames: 3,
            frame_offset: 17,
            end_of_track,
            ..Default::default()
        },
        sample_buffer(&pools, &[0.25; 7]),
    );
    let head = Arc::new(AtomicU64::new(0));
    let raw = RawSource {
        head: Arc::clone(&head),
        chunks: VecDeque::from([invalid]),
    };
    let mut source = source_stage(&pools, raw, AudioSpec::new(2, rate));
    flush_deferred(&mut source);
    assert!(matches!(
        source.step_track(),
        TrackStep::Failed(TrackFailureKind::Render)
    ));
    assert_eq!(
        head.load(Ordering::Acquire),
        20,
        "the malformed packet was pulled"
    );
    assert!(matches!(
        source.step_track(),
        TrackStep::Failed(TrackFailureKind::Render)
    ));
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
        .pitch(Pitch::HELD)
        .build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let mut source = WarpSource::new(
        raw,
        renderer,
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
#[kithara::hang_watchdog]
fn upstream_terminal_failure_keeps_its_classification_after_buffered_pcm(
    #[case] backend: StretchKind,
    #[case] staged: bool,
    quarter: Vec<f32>,
) {
    let pools = pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test sample rate"));
    let failure = TrackFailureKind::RecreateFailed { offset: 91 };
    let frames = match backend {
        #[cfg(feature = "stretch-bungee")]
        StretchKind::Bungee => 8192,
        _ => 4096,
    };
    let quarter = quarter.repeat(2);
    let raw = FailedSource {
        failure,
        chunks: VecDeque::from([
            chunk_with_frames(&pools, spec, 0, frames, &quarter),
            chunk_with_frames(&pools, spec, u64::from(frames), 17, &quarter),
        ]),
    };
    let config = kithara_warp::WarpConfig::builder()
        .backend(backend)
        .speed(0.5)
        .pitch(Pitch::HELD)
        .render_quantum_frames(NonZeroUsize::new(128).expect("test quantum"))
        .build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let mut source = WarpSource::new(
        raw,
        renderer,
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
        hang_tick!();
        flush_deferred(&mut source);
        assert_eq!(source.warp.requires_staging(), staged);
        let step = source.step_track();
        staged_prefix_seen |= source.pending_input.as_ref().is_some_and(|pending| {
            pending.consumed_frames > 0 && pending.consumed_frames < pending.chunk.frames()
        });
        match step {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                hang_reset!();
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
    assert!(
        staged_prefix_seen,
        "a decoded chunk longer than the quantum renders across several packets"
    );
    for _ in 0..3 {
        hang_tick!();
        flush_deferred(&mut source);
        assert!(matches!(source.step_track(), TrackStep::Failed(actual) if actual == failure));
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
    let mut source = source_stage_with_quantum(&pools, source, spec, quantum_frames);
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
fn discontinuity_refreshes_spec_without_resetting_same_revision() {
    let initial = AudioSpec::new(2, NonZeroU32::new(44_100).expect("initial rate"));
    let changed = AudioSpec::new(1, NonZeroU32::new(48_000).expect("changed rate"));
    let pools = pools();
    let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(7, initial)));
    let source = RevisionSource {
        chunks: VecDeque::new(),
        discontinuity: Arc::clone(&discontinuity),
    };
    let mut source = source_stage(&pools, source, initial);
    source.drain_state = DrainState::Exhausted;

    *discontinuity.lock() = SourceDiscontinuity::new(7, changed);
    flush_deferred(&mut source);
    assert_eq!(
        source.discontinuity().map(|stamp| *stamp.spec()),
        Some(changed)
    );
    assert!(matches!(source.drain_state, DrainState::Exhausted));

    *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
    flush_deferred(&mut source);
    assert!(matches!(source.drain_state, DrainState::Open));
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
    let mut source = source_stage(&pools, source, initial);

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
fn live_unity_hands_over_within_its_quantum_and_a_seek_discards_the_held_source(
    half: Vec<f32>,
    quarter: Vec<f32>,
    three_quarter: Vec<f32>,
) {
    for backend in keylock_backends() {
        const ACTIVE_FRAMES: u32 = 4096;
        const UNITY_FRAMES: u32 = 8192;
        const HELD_FRAMES: u32 = 2048;
        // One whole lane packet (the render quantum), so the post-seek chunk
        // renders as itself, without a copy.
        const SENTINEL_FRAMES: u32 = 4096;

        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
        let pools = pools();
        let active_end = u64::from(ACTIVE_FRAMES);
        let unity_end = active_end.saturating_add(u64::from(UNITY_FRAMES));
        let held_end = unity_end.saturating_add(u64::from(HELD_FRAMES));
        let sentinel_end = held_end.saturating_add(u64::from(SENTINEL_FRAMES));

        let active = chunk_with_frames(&pools, spec, 0, ACTIVE_FRAMES, &quarter);
        let unity_input = half.repeat(2);
        let unity = chunk_with_frames(&pools, spec, active_end, UNITY_FRAMES, &unity_input);
        let held = chunk_with_frames(&pools, spec, unity_end, HELD_FRAMES, &half);
        let sentinel_input = three_quarter.repeat(2);
        let mut sentinel =
            chunk_with_frames(&pools, spec, held_end, SENTINEL_FRAMES, &sentinel_input);
        let sentinel_origin = 44_100;
        sentinel.meta.source_span = SourceSpan::new(
            sentinel_origin,
            sentinel_origin + u64::from(SENTINEL_FRAMES),
            spec.sample_rate,
            u64::from(SENTINEL_FRAMES),
        );
        let sentinel_ptr = sentinel.samples.as_ptr();
        let sentinel_samples = sentinel.samples.to_vec();

        let head = Arc::new(AtomicU64::new(0));
        let raw = RawSource {
            chunks: VecDeque::from([active, unity, held, sentinel]),
            head: Arc::clone(&head),
        };
        let config = kithara_warp::WarpConfig::builder()
            .speed(0.5)
            .backend(backend)
            .render_quantum_frames(
                NonZeroUsize::new(usize::try_from(ACTIVE_FRAMES).expect("quantum fits usize"))
                    .expect("test quantum is non-zero"),
            )
            .build();
        let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
        let (mut lane, inbox) = channel::<LaneProtocol>(ChannelConfig::builder().build());
        let mut source = WarpSource::new(
            raw,
            renderer,
            spec,
            pools.clone(),
            LaneSetup {
                inbox,
                preload_chunks: NonZeroUsize::new(1).expect("preload"),
                declick: consts::DEFAULT_DECLICK,
            },
        );

        let mut unity_rate = false;
        let mut passthrough = false;
        for _ in 0..32 {
            if head.load(Ordering::Acquire) == held_end {
                break;
            }
            flush_deferred(&mut source);
            let step = source.step_track();
            assert!(
                !source.warp.transition_pending(),
                "queued unity hands over within the quantum that queued it"
            );
            let TrackStep::Produced(Fetch::Data { data, .. }) = step else {
                continue;
            };
            let span = data.meta.source_span.expect("published mapping");
            passthrough |= unity_rate
                && u64::try_from(data.frames())
                    .is_ok_and(|frames| span.end() - span.start() == frames);
            if !unity_rate {
                lane.send(When::Next, speed_batch(1.0))
                    .expect("unity batch");
                unity_rate = true;
            }
        }
        assert!(passthrough, "the active varispeed run hands over to unity");
        assert_eq!(head.load(Ordering::Acquire), held_end);
        assert!(
            source.pending_input.is_some()
                || source
                    .render_input
                    .as_ref()
                    .is_some_and(|input| !input.is_empty()),
            "the lane holds decoded source for its next window"
        );

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
        assert_eq!(head.load(Ordering::Acquire), held_end);
        assert!(source.pending_input.is_none());
        assert!(
            source
                .render_input
                .as_ref()
                .is_none_or(|input| input.is_empty())
        );

        flush_deferred(&mut source);
        let resumed = match source.step_track() {
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
        let mut source = source_stage_with_quantum(&pools, raw, spec, FRAMES);
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
fn projected_keylock_switch_keeps_pending_input_during_resident_output(quarter: Vec<f32>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("sample rate"));
    let pools = pools();
    let config = kithara_warp::WarpConfig::builder()
        .speed(2.0 / 3.0)
        .pitch(Pitch::HELD)
        .backend(StretchKind::Signalsmith)
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    let head = Arc::new(AtomicU64::new(0));
    let raw = RawSource {
        head: Arc::clone(&head),
        chunks: (0..8)
            .map(|index| chunk_with_frames(&pools, spec, index * 4096, 4096, &quarter))
            .collect(),
    };
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let mut source = WarpSource::new(
        raw,
        renderer,
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
    source.warp.set_pitch(Pitch::default());
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

/// Steps a source like a lane and checks that its rendered source spans
/// follow one another.
#[derive(Default)]
struct ContiguousPlay {
    backend: StretchKind,
    end: Option<u64>,
    produced: usize,
    staged_end: Option<u64>,
}

impl ContiguousPlay {
    /// Steps `source` once and returns the source span it rendered.
    fn step(&mut self, source: &mut WarpSource<RawSource, TestPools>) -> Option<SourceSpan> {
        flush_deferred(source);
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                let span = data.meta.source_span.expect("rendered source mapping");
                assert!(
                    self.end.is_none_or(|end| span.start() == end),
                    "{}: the output source jumps from {:?} to {}",
                    self.backend,
                    self.end,
                    span.start()
                );
                self.end = Some(span.end());
                self.produced += 1;
                Some(span)
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => None,
            TrackStep::Failed(failure) => panic!(
                "{}: the source failed with {failure:?}; keylock released over a window \
                 staged up to {:?}",
                self.backend, self.staged_end
            ),
            _ => panic!("{}: the source must keep playing", self.backend),
        }
    }
}

#[kithara::test(native)]
fn a_keylock_release_over_a_partly_staged_window_keeps_the_source_playing(quarter: Vec<f32>) {
    let backends: Vec<_> = keylock_backends().collect();
    assert!(!backends.is_empty(), "the build compiles a keylock backend");
    for backend in backends {
        let pools = pools();
        let (mut source, mut lane) = lane_over(&pools, 8, |_| &quarter, 0.8, (backend, true));
        let mut play = ContiguousPlay {
            backend,
            ..ContiguousPlay::default()
        };
        let mut steps = 0..1024;
        let staged = steps.by_ref().any(|_| {
            play.step(&mut source);
            play.produced >= 4
                && source.pending_input.is_none()
                && source
                    .render_input
                    .as_ref()
                    .is_some_and(|input| !input.is_empty())
        });
        assert!(
            staged,
            "{backend}: a decoded chunk's tail must sit in a partly staged window"
        );
        let staged_end = source.source.head.load(Ordering::Acquire);
        play.staged_end = Some(staged_end);
        let release = lane
            .send(
                When::Next,
                command_batch(LaneCommand::SetPitch(Pitch::default())),
            )
            .expect("the lane has room for the release");
        let played_past = steps.any(|_| {
            play.step(&mut source)
                .is_some_and(|span| span.end() > staged_end)
        });
        assert!(
            lane.receipts().any(|receipt| receipt.seq() == release
                && matches!(receipt.outcome(), Outcome::Applied { .. })),
            "{backend}: the lane applies the release over the partly staged window"
        );
        assert!(
            played_past,
            "{backend}: the released source must render past the partly staged window"
        );
    }
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
            .pitch(Pitch::HELD)
            .backend(backend)
            .build();
        let target_pools = pools_with_budget(0);
        let renderer = kithara_warp::Warp::new((), &config).renderer(spec, target_pools.clone());
        let mut source = WarpSource::new(
            raw,
            renderer,
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
