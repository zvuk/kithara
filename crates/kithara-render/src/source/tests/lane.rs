use super::*;

#[kithara::test]
fn exhausted_drain_stays_terminal_for_the_decode_epoch() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
    let pools = pools();
    let steps = Arc::new(AtomicU64::new(0));
    let flushes = Arc::new(AtomicU64::new(0));
    let resets = Arc::new(AtomicU64::new(0));
    let discontinuity = Arc::new(Mutex::new(None));
    let source = CountingEofSource {
        discontinuity: Arc::clone(&discontinuity),
        steps: Arc::clone(&steps),
    };
    let effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(CountingEmptyTail {
        flushes: Arc::clone(&flushes),
        resets: Arc::clone(&resets),
    })];
    let mut source = source_stage(&pools, source, effects, spec);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    for _ in 0..3 {
        assert!(matches!(source.step_track(), TrackStep::Eof));
    }

    assert_eq!(steps.load(Ordering::Acquire), 1);
    assert_eq!(flushes.load(Ordering::Acquire), 1);

    assert!(matches!(
        source.source.seek(Duration::from_secs(1)),
        Ok(SeekOutcome::Landed { .. })
    ));
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert_eq!(steps.load(Ordering::Acquire), 2);
    assert_eq!(flushes.load(Ordering::Acquire), 2);
    assert_eq!(resets.load(Ordering::Acquire), 1);

    *discontinuity.lock() = Some(SourceDiscontinuity::new(1, spec));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert_eq!(steps.load(Ordering::Acquire), 2);
    assert_eq!(flushes.load(Ordering::Acquire), 2);
    assert_eq!(resets.load(Ordering::Acquire), 1);

    *discontinuity.lock() = Some(SourceDiscontinuity::new(2, spec));
    flush_deferred(&mut source);
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert_eq!(steps.load(Ordering::Acquire), 3);
    assert_eq!(flushes.load(Ordering::Acquire), 3);
    assert_eq!(resets.load(Ordering::Acquire), 2);
}

#[kithara::test]
fn a_speed_batch_applies_on_its_lane_frame(quarter: Vec<f32>) {
    const AT: u64 = 1_000;
    let pools = pools();
    let (mut source, mut lane) = speed_lane(&pools, &quarter);
    let seq = lane
        .send(
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: AT,
            }),
            speed_batch(1.25),
        )
        .expect("the lane has room for one batch");

    let emitted = emit(&mut source, 0, AT + 2_000);

    if StretchKind::default().capabilities().is_empty() {
        for chunk in &emitted {
            assert_eq!(chunk.source_start, chunk.lane_start);
            assert_eq!(&chunk.samples[..], &quarter[..chunk.samples.len()]);
        }
    }
    let boundary = emitted
        .iter()
        .position(|chunk| chunk.lane_start == AT)
        .expect("a quantum starts on the batch's frame");
    assert!(
        emitted[..boundary].iter().all(|chunk| chunk.revision == 0),
        "frames before the batch render under the initial speed"
    );
    assert!(
        emitted[boundary..]
            .iter()
            .all(|chunk| chunk.revision == seq.get()),
        "frames from the batch on render under it"
    );
    let outcomes = lane
        .receipts()
        .map(|receipt| {
            let seq = receipt.seq();
            let (outcome, _) = receipt.into();
            (seq, outcome)
        })
        .collect::<Vec<(_, Outcome<LaneProtocol>)>>();
    assert!(
        matches!(
            outcomes.as_slice(),
            [(applied, Outcome::Applied { at: LaneFrame { segment: SegmentId::FIRST, frame: AT }, .. })] if *applied == seq
        ),
        "the receipt names the batch's frame: {outcomes:?}"
    );
}

#[kithara::test]
fn a_batch_for_a_rendered_lane_frame_comes_back_late(quarter: Vec<f32>) {
    let pools = pools();
    let (mut source, mut lane) = speed_lane(&pools, &quarter);
    let _ = emit(&mut source, 0, 1_000);

    lane.send(
        When::At(LaneFrame {
            segment: SegmentId::FIRST,
            frame: 500,
        }),
        speed_batch(1.25),
    )
    .expect("the lane has room for one batch");
    let emitted = emit(&mut source, 1_000, 2_000);

    assert!(
        emitted.iter().all(|chunk| chunk.revision == 0),
        "a late batch renders nothing"
    );
    let outcomes = lane
        .receipts()
        .map(|receipt| <(Outcome<LaneProtocol>, _)>::from(receipt).0)
        .collect::<Vec<_>>();
    assert!(
        matches!(outcomes.as_slice(), [Outcome::Rejected(Rejection::Late)]),
        "a rendered frame answers late: {outcomes:?}"
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_speed_change_continues_the_source_at_the_new_step(quarter: Vec<f32>) {
    const AT: u64 = 1_000;
    const SPEED: f64 = 1.25;
    let pools = pools();
    let (mut source, mut lane) = speed_lane(&pools, &quarter);
    lane.send(
        When::At(LaneFrame {
            segment: SegmentId::FIRST,
            frame: AT,
        }),
        speed_batch(1.25),
    )
    .expect("the lane has room for one batch");

    let emitted = emit(&mut source, 0, AT + 4_000);

    for chunk in &emitted {
        let expected = if chunk.lane_start < AT {
            chunk.lane_start
        } else {
            let elapsed: f64 = (chunk.lane_start - AT).as_();
            let stretched: u64 = (elapsed * SPEED).round().as_();
            AT + stretched
        };
        assert!(
            chunk.source_start.abs_diff(expected) <= 1,
            "lane frame {} renders source frame {}, not {expected}",
            chunk.lane_start,
            chunk.source_start
        );
    }
}

/// A quantum at a speed other than unity cannot end on an arbitrary frame
/// by rounding whole source frames; the lane still lands it on the
/// batch's frame, so the batch applies there.
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_speed_batch_lands_on_its_frame_from_any_speed(quarter: Vec<f32>) {
    const AT: u64 = 1_000;
    let pools = pools();
    let (mut source, mut lane) = lane_over(
        &pools,
        3,
        |_| &quarter,
        0.8,
        (StretchKind::default(), false),
    );
    lane.send(
        When::At(LaneFrame {
            segment: SegmentId::FIRST,
            frame: AT,
        }),
        speed_batch(1.25),
    )
    .expect("the lane has room for one batch");

    let emitted = emit(&mut source, 0, AT + 1_000);

    assert!(
        emitted.iter().any(|chunk| chunk.lane_start == AT),
        "a quantum starts on the batch's frame: {:?}",
        emitted
            .iter()
            .map(|chunk| chunk.lane_start)
            .collect::<Vec<_>>()
    );
}

/// A keylock speed change that lands while the previous change's tail
/// still fades out fades from what sounds on its frame, so the output
/// never jumps: no step between two samples exceeds three times the
/// sine's largest step.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn a_speed_change_within_a_tail_fade_fades_from_what_sounds() {
    for backend in keylock_backends() {
        const ENGAGE: u64 = 1_024;
        const FIRST: u64 = 4_099;
        const SECOND: u64 = FIRST + 512;
        const SETTLE: u64 = 16_384;
        let largest_step: f32 = (std::f64::consts::TAU * 440.0 / 44_100.0 * 0.5).as_();
        let signal = sine(12 * consts::LANE_CHUNK_FRAMES as usize);
        let pools = pools();
        let (mut source, mut lane) = stretch_lane(&pools, (backend, true), &signal);
        for (at, speed) in [(ENGAGE, 0.8), (FIRST, 1.25), (SECOND, 0.94)] {
            lane.send(
                When::At(LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: at,
                }),
                speed_batch(speed),
            )
            .expect("the lane has room for each change");
        }

        let pcm = lane_pcm(&emit(&mut source, 0, SECOND + SETTLE));

        let (frame, step) = pcm[pcm_index(FIRST) - 256..pcm_index(SECOND + SETTLE)]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .enumerate()
            .fold((0, 0.0_f32), |worst, (offset, step)| {
                if step > worst.1 {
                    (offset, step)
                } else {
                    worst
                }
            });
        assert!(
            step <= 3.0 * largest_step,
            "{backend:?}: the output jumps {step:.3} at lane frame {} (the sine steps at most \
             {largest_step:.3}); changes at {FIRST} and {SECOND}",
            pcm_index(FIRST) - 256 + frame,
        );
    }
}

/// A keylock engine that changes speed on lane frame X renders from X on
/// what an engine started at X's source frame with the new speed renders:
/// the change lands on its frame and the source does not jump. The old
/// engine's tail fades out within `SETTLE` frames.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn a_keylock_speed_change_renders_on_as_an_engine_started_on_its_frame() {
    for backend in keylock_backends() {
        const ENGAGE: u64 = 1_024;
        /// At 0.8 the 3075 frames from ENGAGE play 2460 source frames whole.
        const AT: u64 = 4_099;
        const SETTLE: u64 = 16_384;
        const WINDOW: usize = 4_096;
        const REACH: usize = 2_048;
        let signal = chirp(12 * consts::LANE_CHUNK_FRAMES as usize);
        let pools = pools();
        let (mut changed, mut lane) = stretch_lane(&pools, (backend, true), &signal);
        lane.send(
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: ENGAGE,
            }),
            speed_batch(0.8),
        )
        .expect("the lane has room for the engaging batch");
        lane.send(
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: AT,
            }),
            speed_batch(1.25),
        )
        .expect("the lane has room for the change");
        let emitted = emit(&mut changed, 0, AT + SETTLE + WINDOW as u64);
        let cue = ENGAGE + (AT - ENGAGE) * 4 / 5;
        assert_eq!(
            emitted
                .iter()
                .find(|chunk| chunk.lane_start == AT)
                .map(|chunk| chunk.source_start),
            Some(cue),
            "the speed change starts on its exact lane and source frame",
        );

        let (mut started, mut fresh) = stretch_lane(&pools, (backend, false), &signal);
        fresh
            .send(
                When::At(LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: ENGAGE,
                }),
                speed_batch(0.8),
            )
            .expect("the lane has room for the reference history");
        let mut start = speed_batch(1.25);
        start.commands.push(LaneCommand::SetKeylock(true));
        fresh
            .send(
                When::At(LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: AT,
                }),
                start,
            )
            .expect("the lane has room for the start");
        let reference = emit(&mut started, 0, AT + SETTLE + (WINDOW + REACH) as u64);
        assert_eq!(
            reference
                .iter()
                .find(|chunk| chunk.lane_start == AT)
                .map(|chunk| chunk.source_start),
            Some(cue),
            "the fresh engine starts on the same lane and source frame",
        );

        let rendered = &lane_pcm(&emitted)[pcm_index(AT + SETTLE)..][..WINDOW];
        let (offset, correlation) = alignment(
            rendered,
            &lane_pcm(&reference),
            pcm_index(AT + SETTLE),
            REACH,
        );
        assert!(
            offset == 0 && correlation > 0.95,
            "{backend:?}: after the change at lane frame {AT} (source {cue}) the lane \
             renders {offset} frames off a fresh engine, correlation {correlation:.3}"
        );
    }
}

/// A batch that changes the engine on lane frame X renders from X on what
/// the new engine started at X's source frame renders: the engine changes
/// on its frame, the source does not jump, and the receipt names X. The
/// old engine's tail fades out within `SETTLE` frames.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn an_engine_batch_renders_on_as_its_engine_started_on_its_frame() {
    const ENGAGE: u64 = 1_024;
    /// At 1.25 the 3072 frames from ENGAGE play 3840 source frames whole.
    const AT: u64 = 4_096;
    const SETTLE: u64 = 16_384;
    const WINDOW: usize = 4_096;
    const REACH: usize = 2_048;
    for backend in keylock_backends() {
        for next in keylock_backends() {
            let (from, command) = if backend == next {
                ((backend, false), LaneCommand::SetKeylock(true))
            } else {
                ((backend, true), LaneCommand::SetBackend(next))
            };
            let to = (next, true);
            let signal = chirp(12 * consts::LANE_CHUNK_FRAMES as usize);
            let pools = pools();
            let (mut changed, mut lane) = stretch_lane(&pools, from, &signal);
            lane.send(
                When::At(LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: ENGAGE,
                }),
                speed_batch(1.25),
            )
            .expect("the lane has room for the engaging batch");
            let seq = lane
                .send(
                    When::At(LaneFrame {
                        segment: SegmentId::FIRST,
                        frame: AT,
                    }),
                    command_batch(command.clone()),
                )
                .expect("the lane has room for the change");
            let emitted = emit(&mut changed, 0, AT + SETTLE + WINDOW as u64);
            let cue = ENGAGE + (AT - ENGAGE) * 5 / 4;
            assert_eq!(
                emitted
                    .iter()
                    .find(|chunk| chunk.lane_start == AT)
                    .map(|chunk| chunk.source_start),
                Some(cue),
                "the engine change starts on its exact lane and source frame",
            );

            let (mut started, mut fresh) = stretch_lane(&pools, (next, false), &signal);
            fresh
                .send(
                    When::At(LaneFrame {
                        segment: SegmentId::FIRST,
                        frame: ENGAGE,
                    }),
                    speed_batch(1.25),
                )
                .expect("the lane has room for the reference history");
            let mut start = speed_batch(1.25);
            start.commands.push(LaneCommand::SetKeylock(true));
            fresh
                .send(
                    When::At(LaneFrame {
                        segment: SegmentId::FIRST,
                        frame: AT,
                    }),
                    start,
                )
                .expect("the lane has room for the start");
            let reference = emit(&mut started, 0, AT + SETTLE + (WINDOW + REACH) as u64);
            assert_eq!(
                reference
                    .iter()
                    .find(|chunk| chunk.lane_start == AT)
                    .map(|chunk| chunk.source_start),
                Some(cue),
                "the fresh engine starts on the same lane and source frame",
            );

            let rendered = &lane_pcm(&emitted)[pcm_index(AT + SETTLE)..][..WINDOW];
            let (offset, correlation) = alignment(
                rendered,
                &lane_pcm(&reference),
                pcm_index(AT + SETTLE),
                REACH,
            );
            assert!(
                offset == 0 && correlation > 0.95,
                "{command:?}: after the change at lane frame {AT} (source {cue}) the lane \
                 renders {offset} frames off a fresh {to:?} engine, correlation {correlation:.3}"
            );
            assert!(
                lane.receipts().any(|receipt| receipt.seq() == seq
                    && matches!(
                        receipt.outcome(),
                        Outcome::Applied {
                            at: LaneFrame {
                                segment: SegmentId::FIRST,
                                frame: AT
                            },
                            ..
                        }
                    )),
                "the change's receipt names its frame"
            );
        }
    }
}

/// An engine batch and a speed batch on the same lane frame X of a lane at
/// unity render from X on what the new engine started at X with the new
/// speed renders: the old engine never renders the new speed.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn an_engine_and_a_speed_batch_on_one_frame_engage_the_new_engine() {
    for backend in keylock_backends() {
        const AT: u64 = 4_096;
        const SETTLE: u64 = 16_384;
        const WINDOW: usize = 4_096;
        const REACH: usize = 2_048;
        let signal = chirp(12 * consts::LANE_CHUNK_FRAMES as usize);
        let pools = pools();
        let (mut changed, mut lane) = stretch_lane(&pools, (backend, false), &signal);
        lane.send(
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: AT,
            }),
            command_batch(LaneCommand::SetKeylock(true)),
        )
        .expect("the lane has room for the engine change");
        lane.send(
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: AT,
            }),
            speed_batch(1.25),
        )
        .expect("the lane has room for the speed change");
        let emitted = emit(&mut changed, 0, AT + SETTLE + WINDOW as u64);

        let (mut started, mut fresh) = stretch_lane(&pools, (backend, true), &signal);
        fresh
            .send(
                When::At(LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: AT,
                }),
                speed_batch(1.25),
            )
            .expect("the lane has room for the start");
        let reference = emit(&mut started, 0, AT + SETTLE + (WINDOW + REACH) as u64);

        let rendered = &lane_pcm(&emitted)[pcm_index(AT + SETTLE)..][..WINDOW];
        let (offset, correlation) = alignment(
            rendered,
            &lane_pcm(&reference),
            pcm_index(AT + SETTLE),
            REACH,
        );
        assert!(
            offset == 0 && correlation > 0.95,
            "{backend:?}: after keylock and speed at lane frame {AT} the lane renders \
             {offset} frames off a fresh keylock engine, correlation {correlation:.3}"
        );
    }
}
#[cfg(feature = "stretch-identity")]
#[kithara::test]
fn identity_source_preserves_buffers_despite_initial_and_live_controls(quarter: Vec<f32>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("sample rate"));
    let pools = pools();
    let chunks = [
        chunk_with_frames(&pools, spec, 0, 64, &quarter),
        chunk_with_frames(&pools, spec, 64, 64, &quarter),
    ];
    let pointers = chunks.each_ref().map(|chunk| chunk.samples.as_ptr());
    let raw = RawSource {
        chunks: VecDeque::from(chunks),
        head: Arc::new(AtomicU64::new(0)),
    };
    let config = kithara_warp::WarpConfig::builder()
        .backend(StretchKind::Identity)
        .speed(0.5)
        .keylock(true)
        .render_quantum_frames(NonZeroUsize::new(16).expect("quantum"))
        .build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let drain = EffectDrain::new(0, &pools).expect("empty effect drain");
    let (mut lane, inbox) = channel::<LaneProtocol>(ChannelConfig::builder().build());
    let mut source = WarpSource::new(
        raw,
        renderer,
        Vec::new(),
        drain,
        spec,
        pools.clone(),
        LaneSetup {
            inbox,
            preload_chunks: NonZeroUsize::new(1).expect("preload"),
            declick: consts::DEFAULT_DECLICK,
        },
    );
    for (index, pointer) in pointers.into_iter().enumerate() {
        if index == 1 {
            lane.send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![
                        LaneCommand::SetSpeed(SpeedCurve::Constant(2.0)),
                        LaneCommand::SetKeylock(false),
                    ],
                },
            )
            .expect("the lane has room");
        }
        let TrackStep::Produced(Fetch::Data {
            data, source_end, ..
        }) = source.step_track()
        else {
            panic!("Identity emits each original chunk immediately");
        };
        assert_eq!(data.samples.as_ptr(), pointer);
        assert_eq!(data.spec(), spec);
        assert_eq!(data.frames(), 64);
        assert_eq!(&*data.samples, &quarter[..128]);
        assert_eq!(
            data.meta.frame_offset,
            u64::try_from(index).expect("index") * 64
        );
        assert_eq!(
            source_end,
            Some(SourceEnd::new(
                u64::try_from(index + 1).expect("index") * 64,
                spec.sample_rate,
            ))
        );
        assert!(!source.warp.requires_staging());
        flush_deferred(&mut source);
    }
    let mut ended = false;
    for _ in 0..8 {
        match source.step_track() {
            TrackStep::StateChanged => flush_deferred(&mut source),
            TrackStep::Eof => {
                ended = true;
                break;
            }
            _ => panic!("Identity owns no tail and finishes without failure"),
        }
    }
    assert!(ended, "Identity reaches EOF");
}

#[cfg(all(
    feature = "stretch-identity",
    any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee",
        feature = "stretch-glide"
    )
))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
fn scheduled_identity_native_changes_preserve_the_decoded_suffix(
    #[case] backend: StretchKind,
    quarter: Vec<f32>,
) {
    const ENGAGE: u64 = 1_000;
    const RETURN: u64 = 2_000;
    // A scheduled 2x landing rounds 1,999 source frames to 1,000 output frames.
    const RATE_SOURCE_FRAMES: u64 = 1_999;
    let pools = pools();
    let (mut source, mut lane) =
        lane_over(&pools, 3, |_| &quarter, 2.0, (StretchKind::Identity, false));
    let first = lane
        .send(
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: ENGAGE,
            }),
            command_batch(LaneCommand::SetBackend(backend)),
        )
        .expect("the lane has room");
    let second = lane
        .send(
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: RETURN,
            }),
            command_batch(LaneCommand::SetBackend(StretchKind::Identity)),
        )
        .expect("the lane has room");
    let expected_output =
        u64::from(3 * consts::LANE_CHUNK_FRAMES) - RATE_SOURCE_FRAMES + (RETURN - ENGAGE);
    let emitted = emit(&mut source, 0, expected_output);
    let actual_frames = emitted
        .iter()
        .map(|chunk| chunk.samples.len() / 2)
        .sum::<usize>();
    assert_eq!(
        actual_frames,
        usize::try_from(expected_output).expect("output frames")
    );
    for chunk in &emitted {
        let expected_source = if chunk.lane_start < ENGAGE {
            chunk.lane_start
        } else if chunk.lane_start < RETURN {
            ENGAGE + (chunk.lane_start - ENGAGE) * 2
        } else {
            chunk.lane_start + RATE_SOURCE_FRAMES - (RETURN - ENGAGE)
        };
        if (ENGAGE..RETURN).contains(&chunk.lane_start) {
            assert!(
                chunk.source_start.abs_diff(expected_source) <= 1,
                "backend changes preserve source position at lane frame {}: {} vs {expected_source}",
                chunk.lane_start,
                chunk.source_start
            );
        } else {
            assert_eq!(chunk.source_start, expected_source);
        }
        if chunk.lane_start >= RETURN {
            assert!(
                chunk.samples.iter().all(|sample| *sample == quarter[0]),
                "the held Identity suffix remains unchanged"
            );
        }
    }
    assert_eq!(
        source.source.head.load(Ordering::Acquire),
        u64::from(3 * consts::LANE_CHUNK_FRAMES)
    );
    assert!(
        source.pending_input.is_none(),
        "every decoded suffix was consumed"
    );
    assert_eq!(
        source.warp.rendered_source_end().map(|(frame, _)| frame),
        Some(u64::from(3 * consts::LANE_CHUNK_FRAMES))
    );
    let mut ended = false;
    for _ in 0..8 {
        match source.step_track() {
            TrackStep::StateChanged => flush_deferred(&mut source),
            TrackStep::Eof => {
                ended = true;
                break;
            }
            _ => panic!("the complete Identity suffix leaves no output tail"),
        }
    }
    assert!(ended, "the complete source reaches EOF");
    let outcomes = lane
        .receipts()
        .map(|receipt| {
            let seq = receipt.seq();
            let (outcome, _) = receipt.into();
            (seq, outcome)
        })
        .collect::<Vec<_>>();
    assert!(matches!(outcomes.as_slice(), [
        (engage, Outcome::Applied { at: LaneFrame { segment: SegmentId::FIRST, frame: ENGAGE }, .. }),
        (identity, Outcome::Applied { at: LaneFrame { segment: SegmentId::FIRST, frame: RETURN }, .. }),
    ] if *engage == first && *identity == second));
}
