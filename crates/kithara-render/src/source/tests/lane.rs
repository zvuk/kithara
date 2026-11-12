use super::*;

#[kithara::test]
fn exhausted_drain_stays_terminal_for_the_decode_epoch() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
    let pools = pools();
    let steps = Arc::new(AtomicU64::new(0));
    let discontinuity = Arc::new(Mutex::new(None));
    let source = CountingEofSource {
        discontinuity: Arc::clone(&discontinuity),
        steps: Arc::clone(&steps),
    };
    let mut source = source_stage(&pools, source, spec);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    for _ in 0..3 {
        assert!(matches!(source.step_track(), TrackStep::Eof));
    }

    assert_eq!(steps.load(Ordering::Acquire), 1);

    assert!(matches!(
        source.source.seek(Duration::from_secs(1)),
        Ok(SeekOutcome::Landed { .. })
    ));
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert_eq!(steps.load(Ordering::Acquire), 2);

    *discontinuity.lock() = Some(SourceDiscontinuity::new(1, spec));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert_eq!(steps.load(Ordering::Acquire), 2);

    *discontinuity.lock() = Some(SourceDiscontinuity::new(2, spec));
    flush_deferred(&mut source);
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert!(matches!(source.step_track(), TrackStep::Eof));
    assert_eq!(steps.load(Ordering::Acquire), 3);
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

/// A keylock engine that changes speed on lane frame X changes it there for
/// the listener: the quantum on X starts on X's source frame, a kick sounds
/// where the speeds on either side of X put it, and a tone keeps its level
/// through X.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn a_keylock_speed_change_sounds_on_its_frame_at_its_level() {
    /// At 0.8 the 30000 frames before the change play 24000 source frames whole.
    const AT: u64 = 30_000;
    const CUE: u64 = AT * 4 / 5;
    const ONSET_TOLERANCE: f64 = 240.0;
    let chunks = |signal: &[f32]| {
        u32::try_from(signal.len() / (2 * consts::LANE_CHUNK_FRAMES as usize))
            .expect("test chunk count fits u32")
    };
    let change = || {
        (
            When::At(LaneFrame {
                segment: SegmentId::FIRST,
                frame: AT,
            }),
            speed_batch(1.25),
        )
    };
    for backend in keylock_backends() {
        let signal = kicks();
        let pools = pools();
        let (mut source, mut lane) = lane_over(
            &pools,
            chunks(&signal),
            |index| &signal[index as usize * 2 * consts::LANE_CHUNK_FRAMES as usize..],
            0.8,
            (backend, true),
        );
        let (when, batch) = change();
        lane.send(when, batch)
            .expect("the lane has room for the change");
        let emitted = emit(&mut source, 0, 2 * AT);
        assert_eq!(
            emitted
                .iter()
                .find(|chunk| chunk.lane_start == AT)
                .map(|chunk| chunk.source_start),
            Some(CUE),
            "the speed change starts on its exact lane and source frame",
        );
        let struck: Vec<f32> = signal.iter().step_by(2).copied().collect();
        let rendered = lane_pcm(&emitted);
        let (at, cue): (f64, f64) = (AT.as_(), CUE.as_());
        for kick in [4_800.0, 28_800.0, 52_800.0] {
            let source = kick_onset(&struck, kick);
            let expected = if source < cue {
                source / 0.8
            } else {
                at + (source - cue) / 1.25
            };
            let actual = kick_onset(&rendered, expected);
            assert!(
                (actual - expected).abs() <= ONSET_TOLERANCE,
                "{backend:?}: the kick at source frame {source} sounds at lane frame {actual}, \
                 not {expected:.0}"
            );
        }

        let signal = sine(12 * consts::LANE_CHUNK_FRAMES as usize);
        let (mut source, mut lane) = lane_over(
            &pools,
            chunks(&signal),
            |index| &signal[index as usize * 2 * consts::LANE_CHUNK_FRAMES as usize..],
            0.8,
            (backend, true),
        );
        let (when, batch) = change();
        lane.send(when, batch)
            .expect("the lane has room for the change");
        let rendered = lane_pcm(&emit(&mut source, 0, AT + 8_192));
        let level = tone_level_db(&rendered[pcm_index(AT - 4_096)..], &signal);
        assert!(
            level.abs() <= consts::TONE_LEVEL_DB,
            "{backend:?}: the tone's level moves {level:.2} dB around its speed change"
        );
    }
}

/// Keylock speed changes that each reach the lane one engine latency ahead
/// never retire the engine. A retired engine's tail fades against its
/// re-primed successor, which moves a tone's level; here the tone holds it
/// through every change, and each change still applies on its frame under
/// its own revision.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn keylock_speed_changes_known_a_latency_ahead_keep_the_engine() {
    const SPEEDS: [f32; 3] = [1.25, 0.8, 1.1];
    for backend in keylock_backends() {
        let signal = sine(12 * consts::LANE_CHUNK_FRAMES as usize);
        let pools = pools();
        let chunks = signal.len() / (2 * consts::LANE_CHUNK_FRAMES as usize);
        let (mut source, mut lane) = lane_over(
            &pools,
            u32::try_from(chunks).expect("test chunk count fits u32"),
            |index| &signal[index as usize * 2 * consts::LANE_CHUNK_FRAMES as usize..],
            0.8,
            (backend, true),
        );
        let mut emitted = emit(&mut source, 0, 256);
        let latency =
            u64::try_from(source.engine_latency().get()).expect("engine latency fits u64");
        let spacing = latency + 512;
        let changes: Vec<_> = (1_u64..)
            .zip(SPEEDS)
            .map(|(index, speed)| {
                let frame = index * spacing;
                let seq = lane
                    .send(
                        When::At(LaneFrame {
                            segment: SegmentId::FIRST,
                            frame,
                        }),
                        speed_batch(speed),
                    )
                    .expect("the lane has room for each change");
                (frame, seq)
            })
            .collect();
        let end = spacing * (1 + u64::try_from(SPEEDS.len()).expect("change count"));
        emitted.extend(emit(&mut source, 256, end));

        let rendered = lane_pcm(&emitted);
        let level = tone_level_db(&rendered[pcm_index(spacing)..pcm_index(end)], &signal);
        assert!(
            level.abs() <= consts::TONE_LEVEL_DB,
            "{backend:?}: the tone's level moves {level:.2} dB through its speed changes"
        );
        for (index, &(frame, seq)) in changes.iter().enumerate() {
            let until = changes.get(index + 1).map_or(end, |&(next, _)| next);
            assert!(
                emitted.iter().any(|chunk| chunk.lane_start == frame),
                "{backend:?}: a quantum starts on the change at lane frame {frame}"
            );
            assert!(
                emitted
                    .iter()
                    .filter(|chunk| (frame..until).contains(&chunk.lane_start))
                    .all(|chunk| chunk.revision == seq.get()),
                "{backend:?}: frames from lane frame {frame} on render under its batch"
            );
        }
        let applied: Vec<_> = lane
            .receipts()
            .filter_map(|receipt| match receipt.outcome() {
                Outcome::Applied { at, .. } => Some((at.frame, receipt.seq())),
                Outcome::Rejected(_) => None,
            })
            .collect();
        assert_eq!(
            applied, changes,
            "{backend:?}: each change applies on its frame"
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
        let command = LaneCommand::SetKeylock(true);
        let to = (backend, true);
        let signal = chirp(12 * consts::LANE_CHUNK_FRAMES as usize);
        let pools = pools();
        let (mut changed, mut lane) = stretch_lane(&pools, (backend, false), &signal);
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

        let (mut started, mut fresh) = stretch_lane(&pools, (backend, false), &signal);
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
        // A decoded chunk the size of a lane packet renders as itself.
        .render_quantum_frames(NonZeroUsize::new(64).expect("quantum"))
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
