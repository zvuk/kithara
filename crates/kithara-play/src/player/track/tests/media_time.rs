use super::*;

#[cfg(any(
    feature = "stretch-glide",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee"
))]
fn curve_source(
    inbox: Inbox<LaneProtocol>,
    rate: NonZeroU32,
    origin: u64,
    quantum: std::num::NonZeroUsize,
) -> kithara_render::WarpSource<kithara_render::mock::ScriptedSource, TestPools> {
    let pools = pools();
    let spec = AudioSpec::new(2, rate);
    let resume = spec
        .duration_for(origin + 600)
        .expect("consumed source boundary");
    let prefix = u64::try_from(spec.frames_for(resume).expect("native render prefix").get())
        .expect("native prefix frame fits u64");
    let chunks = [origin, prefix].map(|start| {
        kithara_audio::TrackStep::Produced(kithara_audio::Fetch::data(
            kithara_signal::AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: 8_192,
                    frame_offset: start,
                    timestamp: spec.duration_for(start).expect("source frame"),
                    end_timestamp: spec.duration_for(start + 8_192).expect("source end"),
                    source_span: kithara_signal::SourceSpan::new(
                        start,
                        start + 8_192,
                        spec.sample_rate,
                        8_192,
                    ),
                    visible_origin: Some(Duration::ZERO),
                    media_origin: Some(Duration::ZERO),
                    ..AudioChunkInfo::default()
                },
                kithara_test_utils::bufpool::sample_buffer(&pools, &[0.25; 16_384]),
            ),
        ))
    });
    let backend = *StretchKind::all()
        .iter()
        .find(|backend| {
            backend
                .capabilities()
                .contains(kithara_warp::WarpCapabilities::RATE)
        })
        .expect("a rate backend is enabled");
    let warp = kithara_warp::Warp::new(
        (),
        &kithara_warp::WarpConfig::builder()
            .backend(backend)
            .render_quantum_frames(quantum)
            .build(),
    );
    kithara_render::WarpSource::new(
        kithara_render::mock::ScriptedSource::new(chunks),
        warp.renderer(spec, pools.clone()),
        Vec::new(),
        kithara_effects::EffectDrain::new(0, &pools).expect("drain"),
        spec,
        pools,
        kithara_render::LaneSetup {
            inbox,
            preload_chunks: std::num::NonZeroUsize::MIN,
            declick: kithara_dsp::param::SmootherConfig::default(),
        },
    )
}

#[cfg(any(
    feature = "stretch-glide",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee"
))]
#[kithara::hang_watchdog]
fn next_curve_chunk<T: kithara_audio::AudioSource<Chunk = kithara_signal::AudioChunk>>(
    source: &mut kithara_render::WarpSource<T, TestPools>,
) -> kithara_signal::AudioChunk {
    use kithara_audio::AudioSource;
    loop {
        hang_tick!();
        let _ = source.prepare_deferred();
        source.finish_deferred();
        match source.step_track() {
            kithara_audio::TrackStep::Produced(kithara_audio::Fetch::Data { data, .. }) => {
                source.admitted();
                hang_reset!();
                return data;
            }
            kithara_audio::TrackStep::StateChanged | kithara_audio::TrackStep::Blocked(_) => {}
            _ => panic!("the prepared curve source must produce PCM"),
        }
    }
}

#[cfg(any(
    feature = "stretch-glide",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee"
))]
pub(super) fn assert_quiet_curve_source_span_projection() {
    let frames = std::num::NonZeroU64::new(1_920).expect("curve frames");
    let curves = [
        SpeedCurve::Constant(1.0),
        SpeedCurve::Constant(0.5),
        SpeedCurve::Constant(1.5),
        SpeedCurve::Constant(1.1),
        SpeedCurve::Ramp { to: 2.0, frames },
        SpeedCurve::Steps(Arc::from([(960, 2.0)])),
        SpeedCurve::Steps(Arc::from([(0, 1.25), (960, 2.0)])),
        SpeedCurve::Approach(
            kithara_signal::ExponentialApproach::new(1.25, 2.0, 480.0).expect("explicit origin"),
        ),
        SpeedCurve::ApproachSteps(Arc::from([
            (
                0,
                kithara_signal::ExponentialApproach::new(1.25, 2.0, 480.0).expect("first law"),
            ),
            (
                700,
                kithara_signal::ExponentialApproach::new(1.8, 0.75, 220.5).expect("second law"),
            ),
        ])),
    ];
    for rate in [44_100, 48_000] {
        let rate = NonZeroU32::new(rate).expect("native rate");
        for origin in [0, 1] {
            for quantum in [40, 120] {
                let quantum = std::num::NonZeroUsize::new(quantum).expect("quantum");
                for curve in &curves {
                    assert_quiet_curve_projection(curve, frames, rate, origin, quantum);
                }
            }
        }
    }
}

#[cfg(any(
    feature = "stretch-glide",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee"
))]
#[kithara::hang_watchdog]
fn assert_quiet_curve_projection(
    curve: &SpeedCurve,
    frames: std::num::NonZeroU64,
    rate: NonZeroU32,
    origin: u64,
    quantum: std::num::NonZeroUsize,
) {
    let mut rig = rig();
    let (mut track, inbox) = prepared_track();
    let mut source = curve_source(inbox, rate, origin, quantum);
    track.status = TrackStatus::Playing { since: frame(0) };
    let predecessor = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::SetSpeed {
            speed: SpeedCurve::Ramp { to: 3.0, frames },
            at: When::Next,
        },
    )
    .expect("native in-segment speed");
    let mut consumed = None;
    while source.cursor().frame < 1_920 {
        hang_tick!();
        let chunk = next_curve_chunk(&mut source);
        if track.source_basis.is_none() {
            track.source_basis = Some(chunk.meta);
        }
        let end = chunk.meta.lane_frame + u64::from(chunk.meta.frames);
        if end == 480 {
            consumed = Some(SlotMark {
                session: frame(480),
                lane: LaneFrame {
                    segment: chunk.meta.segment,
                    frame: end,
                },
                position: chunk
                    .meta
                    .source_span
                    .expect("native predecessor span")
                    .position_at(u64::from(chunk.meta.frames))
                    .expect("consumed endpoint"),
            });
        }
    }
    rig.with_outbox(|out| track.tick(frame(480), out))
        .expect("native predecessor receipt");
    let consumed = consumed.expect("a consumed predecessor quantum");
    let spec = AudioSpec::new(2, rate);
    assert_eq!(
        consumed.position,
        spec.duration_for(origin + 600)
            .expect("native consumed mark")
    );
    assert_eq!(source.cursor().frame, 1_920);
    assert_eq!(track.snapshot().speed, 3.0);
    track.mark = Some(consumed);
    assert!(
        matches!(track.speed_receipt(), Some(Settled::Applied { seq, at })
            if seq == predecessor && at == frame(0))
    );
    assert!(track.speed_receipt().is_none());
    assert!(track.speed_answers.is_empty());
    track.status = TrackStatus::Paused {
        at: consumed.position,
    };
    track.resume = Some(consumed);
    track.mark = Some(consumed);
    let available = track.lane.as_ref().expect("real lane").available();
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::SetSpeed {
            speed: curve.clone(),
            at: When::Next,
        },
    );
    let segment = track.segment;
    let seq = track.lane_commands.last().expect("native Segment").seq;
    assert_eq!(
        track.lane.as_ref().expect("real lane").available() + 1,
        available
    );
    assert!(
        track
            .lane_commands
            .iter()
            .rev()
            .take_while(|operation| operation.seq == seq)
            .all(|operation| operation.applied().is_none())
    );
    assert_eq!(track.ready, None);
    assert_eq!(track.snapshot().speed, 3.0);
    let mut spans = Vec::new();
    let mut metadata = Vec::new();
    let endpoint = origin + 3_960;
    while source.cursor().segment != segment
        || metadata.last().is_none_or(|meta: &AudioChunkInfo| {
            meta.source_span
                .and_then(|span| span.source_ratio_at(span.output_frames()))
                .is_none_or(|(numerator, denominator)| {
                    numerator / denominator.get() < u128::from(endpoint)
                })
        })
    {
        hang_tick!();
        let chunk = next_curve_chunk(&mut source);
        metadata.push(chunk.meta);

        assert_eq!(chunk.meta.segment, segment);
        spans.push((
            chunk.meta.lane_frame,
            chunk.meta.source_span.expect("actual rendered source law"),
        ));
    }
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("real segment readiness");
    assert_eq!(track.ready, Some(segment));
    let final_speed = match curve {
        SpeedCurve::Constant(speed) | SpeedCurve::Ramp { to: speed, .. } => *speed,
        SpeedCurve::Steps(steps) => steps.last().expect("speed tail").1,
        SpeedCurve::Approach(approach) => approach.target().to_f32().expect("published speed"),
        SpeedCurve::ApproachSteps(steps) => steps
            .last()
            .expect("law tail")
            .1
            .target()
            .to_f32()
            .expect("published speed"),
    };
    assert_eq!(track.snapshot().speed, final_speed);

    assert!(
        track
            .lane_commands
            .iter()
            .filter(|operation| operation.seq == seq)
            .all(|operation| operation.applied() == Some(true))
    );
    assert!(
        track
            .speed_answers
            .iter()
            .all(|(answered, _)| *answered != seq)
    );
    assert!(track.speed_receipt().is_none());
    track.observed_source = metadata.first().copied();
    let initial = spans
        .first()
        .expect("first new quantum")
        .1
        .position_at(0)
        .expect("physical initial anchor");
    assert_eq!(initial, consumed.position);
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame { segment, frame: 0 },
        position: initial,
    });
    track.duration = Some(spec.duration_for(endpoint).expect("native endpoint"));
    let mut physical_end = None;
    for (start, span) in spans {
        for offset in 0..=span.output_frames() {
            let actual = span.position_at(offset).expect("actual source coordinate");
            let at = start + offset;
            let planned = track
                .planned(frame(i64::try_from(at).expect("session frame")), rate)
                .expect("Player projection")
                .0;
            assert!(
                planned.abs_diff(actual) <= Duration::from_nanos(1),
                "{curve:?} at {at}: planned {planned:?}, source {actual:?}"
            );
            let (numerator, denominator) = span
                .source_ratio_at(offset)
                .expect("exact physical boundary");
            if physical_end.is_none() && numerator / denominator.get() >= u128::from(endpoint) {
                physical_end = Some(frame(i64::try_from(at).expect("physical end")));
            }
        }
    }
    assert_eq!(
        track.planned_end(rate).expect("physical end projection"),
        physical_end,
        "{curve:?}"
    );
    for meta in metadata {
        let span = meta.source_span.expect("observed source geometry");
        for offset in [0, 1, u64::from(meta.frames) / 2, u64::from(meta.frames)] {
            let at = meta.lane_frame + offset;
            if at
                >= u64::try_from(i64::from(physical_end.expect("physical EOF")))
                    .expect("positive EOF")
            {
                continue;
            }
            track.observed_source = Some(meta);
            track.mark = Some(SlotMark {
                session: frame(i64::try_from(at).expect("session mark")),
                lane: LaneFrame { segment, frame: at },
                position: span.position_at(offset).expect("exact consumed position"),
            });
            assert_eq!(
                track.planned_end(rate).expect("interior physical EOF"),
                physical_end,
                "{curve:?}, rate {rate}, source origin {origin}, quantum {quantum}, consumed {at}"
            );
        }
    }
}

#[kithara::test]
#[case::seek(false)]
#[case::cue(true)]
fn public_media_target_is_translated_once_before_the_raw_segment(#[case] cue: bool) {
    let (mut track, mut inbox) = prepared_track();
    track.media_origin = Some(Duration::from_millis(576));
    let target = Position::from_secs(2);
    let mut rig = rig();
    in_pass(&mut rig, frame(0), |out| {
        if cue {
            track.cue(target, SpeedCurve::Constant(1.25), out)
        } else {
            track.apply(TrackCommand::Seek { to: target }, out)
        }
    })
    .expect("known media target");
    let commands = lane_commands(&mut inbox);
    assert!(matches!(
        commands.as_slice(),
        [LaneCommand::Segment { from, .. }] if *from == Duration::from_millis(2_576)
    ));
}

#[kithara::test]
#[case::jump(false)]
#[case::align(true)]
fn timed_media_target_is_translated_once_before_the_raw_lane_batch(#[case] align: bool) {
    let (mut track, mut inbox) = prepared_track();
    track.media_origin = Some(Duration::from_millis(576));
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: track.segment,
            frame: 0,
        },
        position: Duration::from_millis(576),
    });
    let mut rig = rig();
    let target = Position::from_secs(2);
    let at = frame(8_000);
    let command = if align {
        TrackCommand::Align {
            to: target,
            speed: SpeedCurve::Constant(1.25),
            at,
        }
    } else {
        TrackCommand::Jump { to: target, at }
    };
    in_pass(&mut rig, frame(0), |out| track.apply(command, out)).expect("known timed media target");
    inbox.drain();
    let due = inbox
        .next_due(
            LaneFrame {
                segment: track.segment,
                frame: 8_000,
            },
            1,
        )
        .expect("raw lane batch");
    assert!(due.commands().iter().any(|command| {
        matches!(command, LaneCommand::Jump { to } if *to == Duration::from_millis(2_576))
    }));
    due.apply(kithara_render::LaneApplied {
        source_basis: None,
        engine_latency: FrameCount::new(0),
        ready: None,
    });
}

#[kithara::test]
#[case::seek(0)]
#[case::jump(1)]
#[case::align(2)]
#[case::cue(3)]
fn unknown_media_translation_refuses_commands_without_changing_the_snapshot(#[case] command: u8) {
    let (mut track, mut inbox) = prepared_track();
    track.media_origin = None;
    track.position = Position::from_secs(7);
    let before = track.snapshot();
    let mut rig = rig();
    let target = Position::from_secs(2);
    let result = in_pass(&mut rig, frame(0), |out| match command {
        0 => track.apply(TrackCommand::Seek { to: target }, out),
        1 => track.apply(
            TrackCommand::Jump {
                to: target,
                at: frame(8_000),
            },
            out,
        ),
        2 => track.apply(
            TrackCommand::Align {
                to: target,
                speed: SpeedCurve::Constant(1.25),
                at: frame(8_000),
            },
            out,
        ),
        3 => track.cue(target, SpeedCurve::Constant(1.25), out),
        _ => unreachable!("fixture command"),
    });
    assert!(matches!(result, Err(PlayError::Untimed)));
    assert!(lane_commands(&mut inbox).is_empty());
    assert_eq!(track.snapshot().position, before.position);
    assert_eq!(track.snapshot().status, before.status);
    assert_eq!(track.snapshot().mark, before.mark);
}

#[kithara::test]
#[case::inside_prefix(250, 0)]
#[case::past_media_zero(400, 224)]
fn planned_media_position_integrates_the_raw_prefix_before_clamping(
    #[case] at: i64,
    #[case] expected_ms: u64,
) {
    let mut track = track(A);
    track.media_origin = Some(Duration::from_millis(576));
    track.segment_speed = 2.0;
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: track.segment,
            frame: 0,
        },
        position: Position::ZERO,
    });
    let (position, speed) = track
        .planned(
            frame(at),
            NonZeroU32::new(1_000).expect("millisecond clock"),
        )
        .expect("known media projection");
    assert_eq!(position, Duration::from_millis(expected_ms));
    assert_eq!(speed, 2.0);
    assert_eq!(track.mark.expect("raw mark").position, Position::ZERO);
}

#[kithara::test]
#[case::trimmed(576, 2_000)]
#[case::untrimmed(0, 1_424)]
fn public_media_extent_uses_the_same_basis_as_public_position(
    #[case] visible_ms: u64,
    #[case] expected_ms: u64,
) {
    let spec = AudioSpec::new(2, NonZeroU32::new(1_000).expect("millisecond source clock"));
    let mut packets = kithara_render::mock::PacketRing::with_origin(
        spec,
        Duration::from_secs(2),
        1,
        Duration::from_millis(visible_ms),
    );
    let mut pcm = kithara_render::mock::chunk(spec, SegmentId::FIRST, 0, visible_ms, &[0.5; 2]);
    pcm.meta.media_origin = Some(Duration::from_millis(576));
    packets.push(kithara_render::PcmPacket::Chunk(Box::new(pcm)));
    let receiver = packets.receiver.take().expect("prepared source receiver");
    assert_eq!(receiver.duration(), Some(Duration::from_secs(2)));
    let source = receiver.next_chunk_info().expect("immutable source packet");
    let mut rig = rig();
    let output = mock::output(None).get();
    let mut deck = crate::DeckSnapshot {
        slots: vec![SlotSnapshot {
            mark: Some(SlotMark {
                session: frame(0),
                lane: LaneFrame {
                    segment: source.segment,
                    frame: source.lane_frame,
                },
                position: source.timestamp,
            }),
            source: Some(source),
            position: source.timestamp.as_secs_f64(),
            ..Default::default()
        }],
        ..Default::default()
    };
    for mark in [deck.slots[0].mark, None] {
        let mut track = track(A);
        track.attach_at = Some(frame(0));
        deck.slots[0].mark = mark;
        deck.slots[0].source = Some(source);
        {
            let fixture = &mut *rig;
            let mut scope = fixture
                .ring
                .scope(fixture.scope)
                .expect("live source scope");
            let out = Outbox::new(&mut scope, &mut fixture.dispatcher).in_pass(crate::DeckPass {
                mix: DeckMixSettings::default(),
                suspended: false,
                now: frame(0),
                delivery: FrameCount::new(0),
                output: &output,
                deck: &deck,
            });
            track.observe(&out);
            assert_eq!(
                track.snapshot().duration,
                Some(Duration::from_millis(expected_ms))
            );
        }
        deck.slots[0].source = Some(AudioChunkInfo {
            segment: source.segment.next(),
            media_end: Some(Duration::from_secs(999)),
            ..source
        });
        let fixture = &mut *rig;
        let mut scope = fixture
            .ring
            .scope(fixture.scope)
            .expect("live source scope");
        let out = Outbox::new(&mut scope, &mut fixture.dispatcher).in_pass(crate::DeckPass {
            mix: DeckMixSettings::default(),
            suspended: false,
            now: frame(0),
            delivery: FrameCount::new(0),
            output: &output,
            deck: &deck,
        });
        track.observe(&out);
        assert_eq!(
            track.snapshot().duration,
            Some(Duration::from_millis(expected_ms))
        );
    }
    assert_eq!(receiver.duration(), Some(Duration::from_secs(2)));
}

#[kithara::test]
fn planned_media_position_is_untimed_until_its_source_basis_is_known() {
    let mut track = track(A);
    track.media_origin = None;
    track.position = Duration::from_secs(7);
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: track.segment,
            frame: 0,
        },
        position: Duration::from_secs(14),
    });
    assert!(matches!(
        track.planned(frame(1_000), mock::SAMPLE_RATE),
        Err(PlayError::Untimed)
    ));
    assert_eq!(track.snapshot().position, Duration::from_secs(7));
    assert_eq!(track.snapshot().duration, None);
}

#[kithara::test]
#[case::media_position(2_000, 2_576)]
#[case::untrimmed_prefix(0, 250)]
fn host_rate_change_keeps_raw_resume_custody_without_a_media_seek(
    #[case] media_ms: u64,
    #[case] raw_ms: u64,
) {
    let (mut track, mut inbox) = prepared_track();
    track.media_origin = Some(Duration::from_millis(576));
    track.position = Duration::from_millis(media_ms);
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: track.segment,
            frame: 0,
        },
        position: Duration::from_millis(raw_ms),
    });
    let mut rig = rig();
    let rate = NonZeroU32::new(48_000).expect("host rate");
    in_pass(&mut rig, frame(0), |out| {
        track.apply(TrackCommand::SetHostRate { rate }, out)
    })
    .expect("host rate change does not require a media seek");
    let commands = lane_commands(&mut inbox);
    assert!(matches!(
        commands.as_slice(),
        [LaneCommand::SetHostRate { rate: requested, .. }] if *requested == rate
    ));
    assert_eq!(track.snapshot().position, Duration::from_millis(media_ms));
    assert_eq!(track.media_origin, Some(Duration::from_millis(576)));
    #[cfg(any(
        feature = "stretch-glide",
        feature = "stretch-signalsmith",
        feature = "stretch-bungee"
    ))]
    {
        let old_rate = NonZeroU32::new(48_000).expect("old PCM rate");
        let rate = NonZeroU32::new(44_100).expect("new PCM rate");
        let (mut track, inbox) = prepared_track();
        let old_spec = AudioSpec::new(2, old_rate);
        track.source_basis = Some(AudioChunkInfo {
            spec: old_spec,
            frames: 1,
            timestamp: Duration::ZERO,
            end_timestamp: old_spec.duration_for(1).expect("old native frame"),
            ..AudioChunkInfo::default()
        });
        track.status = TrackStatus::Playing { since: frame(0) };
        track.mark = Some(SlotMark {
            session: frame(0),
            lane: LaneFrame {
                segment: track.segment,
                frame: 0,
            },
            position: Duration::ZERO,
        });
        let mut rig = core::rig();
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::SetSpeed {
                speed: SpeedCurve::Constant(0.25),
                at: When::Next,
            },
        )
        .expect("actual preceding speed command");
        command_at_zero(&mut track, &mut rig, TrackCommand::SetHostRate { rate })
            .expect("actual new-generation command");
        let segment = track.segment;
        let mut source = curve_source(
            inbox,
            rate,
            0,
            std::num::NonZeroUsize::new(40).expect("quantum"),
        );
        let chunk = next_curve_chunk(&mut source);
        assert_eq!(chunk.meta.segment, segment);
        assert!(source.is_preloaded());
        rig.with_outbox(|out| track.tick(frame(0), out))
            .expect("actual ready receipt");
        let basis = track.source_basis.expect("prepared native PCM fact");
        assert_eq!(basis.spec.sample_rate, rate);
        assert_eq!(basis.timestamp, Duration::ZERO);
        assert_eq!(
            basis
                .source_span
                .expect("actual native input span")
                .output_frames(),
            8_192
        );
        assert_ne!(
            basis.frames, chunk.meta.frames,
            "the native fact must precede Warp output partitioning"
        );
        track.observed_source = Some(chunk.meta);
        track.duration = Some(
            AudioSpec::new(2, rate)
                .duration_for(1_000)
                .expect("native EOF"),
        );
        track.status = TrackStatus::Playing { since: frame(0) };
        track.mark = Some(SlotMark {
            session: frame(0),
            lane: LaneFrame { segment, frame: 0 },
            position: chunk
                .meta
                .source_span
                .expect("physical new-rate source law")
                .position_at(0)
                .expect("physical origin"),
        });
        assert_eq!(
            track.planned_end(rate).expect("new native EOF projection"),
            Some(frame(4_000))
        );
        track.declick = source.declick_frames();
        let landing = 1 + u64::try_from(track.declick.get()).expect("native declick");
        assert!(landing < 4_000);
        track
            .send_lane(
                LaneCommand::Jump {
                    to: Duration::from_nanos(56_689),
                },
                When::At(LaneFrame { segment, frame: 1 }),
            )
            .expect("future Jump on the new native lattice");
        let expected = i64::try_from(landing + 3_991).expect("exact raw Jump boundary");
        assert_eq!(
            track.planned_end(rate).expect("new native Jump projection"),
            Some(frame(expected))
        );
    }
    #[cfg(all(
        not(target_arch = "wasm32"),
        feature = "symphonia",
        feature = "resample-rubato",
        any(
            feature = "stretch-glide",
            feature = "stretch-signalsmith",
            feature = "stretch-bungee"
        )
    ))]
    if media_ms == 2_000 {
        assert_source_end_resume_endpoint();
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    feature = "symphonia",
    feature = "resample-rubato",
    any(
        feature = "stretch-glide",
        feature = "stretch-signalsmith",
        feature = "stretch-bungee"
    )
))]
#[kithara::hang_watchdog]
fn assert_source_end_resume_endpoint() {
    use kithara_audio::{Audio, AudioConfig, AudioSource, Fetch, SourceEnd, TrackStep};
    use kithara_file::{File, FileConfig, FileSrc};

    let old_rate = NonZeroU32::new(48_000).expect("old host rate");
    let rate = NonZeroU32::new(44_100).expect("new host rate");
    let native_spec = AudioSpec::new(2, rate);
    let old_spec = AudioSpec::new(2, old_rate);
    let pools = pools();
    let directory = TestTempDir::new();
    let path = directory.path().join("source-end-resume.wav");
    mock::write_pcm_wav(&path, &[0.25; 1_764], native_spec).expect("twenty milliseconds of PCM");
    let file = FileConfig::for_src(FileSrc::Local(path))
        .store(AssetStore::builder(pools.clone()).build())
        .extension("wav")
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>, crate::PlaybackResamplerBackend>::for_stream(file)
        .hint("wav".to_owned())
        .host_sample_rate(old_rate)
        .decoder(
            kithara_audio::AudioDecoderConfig::builder()
                .backend(kithara_decode::DecoderBackend::Symphonia)
                .gapless_mode(kithara_decode::GaplessMode::Disabled)
                .build(),
        )
        .build();
    let runtime = kithara_platform::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("offline source runtime");
    let mut audio = runtime
        .block_on(Audio::prepare(
            config,
            Arc::new(kithara_stream::mock::NoopWorkerWake),
            pools.clone(),
        ))
        .expect("real WAV source");
    let old = loop {
        hang_tick!();
        let _ = audio.prepare_deferred();
        audio.finish_deferred();
        match audio.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                hang_reset!();
                break data;
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Failed(failure) | TrackStep::Produced(Fetch::Failure { failure }) => {
                panic!("old generation failed: {failure:?}")
            }
            _ => panic!("old generation must produce PCM"),
        }
    };
    assert_eq!(old.meta.spec.sample_rate, old_rate);
    audio.commit_source_end(SourceEnd::new(1, old_rate), old.meta);

    let (mut track, inbox) = prepared_track();
    track.source_basis = Some(old.meta);
    track.media_origin = audio.media_origin();
    track.duration = audio
        .media_end()
        .zip(track.media_origin)
        .map(|(end, origin)| end.checked_sub(origin).expect("actual old media extent"));
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: track.segment,
            frame: 0,
        },
        position: old_spec.duration_for(1).expect("retained old source end"),
    });
    let mut rig = rig();
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::SetSpeed {
            speed: SpeedCurve::Constant(0.25),
            at: When::Next,
        },
    )
    .expect("actual quarter-speed command");
    command_at_zero(&mut track, &mut rig, TrackCommand::SetHostRate { rate })
        .expect("actual source recreation command");
    let segment = track.segment;
    assert!(track.observed_source.is_none());
    assert!(track.mark.is_none());
    assert!(matches!(track.planned_end(rate), Err(PlayError::Untimed)));
    let backend = *StretchKind::all()
        .iter()
        .find(|backend| {
            backend
                .capabilities()
                .contains(kithara_warp::WarpCapabilities::RATE)
        })
        .expect("rate backend");
    let warp = kithara_warp::Warp::new(
        (),
        &kithara_warp::WarpConfig::builder()
            .backend(backend)
            .render_quantum_frames(std::num::NonZeroUsize::new(40).expect("quantum"))
            .build(),
    );
    let mut source = kithara_render::WarpSource::new(
        audio,
        warp.renderer(old_spec, pools.clone()),
        Vec::new(),
        kithara_effects::EffectDrain::new(0, &pools).expect("no effect tail"),
        old_spec,
        pools,
        kithara_render::LaneSetup {
            inbox,
            preload_chunks: std::num::NonZeroUsize::MIN,
            declick: kithara_dsp::param::SmootherConfig::default(),
        },
    );
    let first = next_curve_chunk(&mut source);
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("real prepared receipt");
    let native = source
        .native_pcm_basis()
        .expect("Audio-owned native PCM basis");
    assert_eq!(native.spec.sample_rate, rate);
    let (landing, denominator) = kithara_audio::source_lattice_position(
        &native,
        old_spec.duration_for(1).expect("retained old source end"),
    )
    .expect("native PCM landing");
    assert_eq!(
        landing,
        denominator.get(),
        "the nearest new native frame is one"
    );
    assert_eq!(native.frame_offset, 1);
    assert_eq!(
        native.timestamp,
        native_spec.duration_for(1).expect("new native frame one")
    );
    let basis = track.source_basis.expect("actual pre-Warp input basis");
    let input = basis
        .source_geometry()
        .expect("retained source-clock input");
    assert_eq!(input.sample_rate(), old_rate);
    assert_eq!(
        input.source_ratio_at(0),
        Some((1, std::num::NonZeroU128::MIN))
    );
    assert_eq!(basis.frame_offset, native.frame_offset);
    assert_eq!(
        kithara_audio::source_lattice_position(&basis, Duration::from_millis(20))
            .expect("physical EOF on the retained input lattice"),
        (
            141_107,
            std::num::NonZeroU128::new(160).expect("retained endpoint phase")
        ),
    );
    let first_span = first.meta.source_span.expect("actual Warp source law");
    assert_eq!(first_span.sample_rate(), rate);
    assert_eq!(
        first_span.source_ratio_at(0),
        Some((
            147,
            std::num::NonZeroU128::new(160).expect("retained raw phase")
        ))
    );
    assert_eq!(source.media_end(), Some(Duration::from_millis(20)));
    track.observed_source = Some(first.meta);
    track.media_origin = source.media_origin();
    track.duration = source
        .media_end()
        .zip(track.media_origin)
        .map(|(end, origin)| end.checked_sub(origin).expect("media extent"));
    track.status = TrackStatus::Playing { since: frame(0) };
    track.mark = Some(SlotMark {
        session: frame(0),
        lane: LaneFrame { segment, frame: 0 },
        position: first
            .meta
            .source_span
            .expect("resumed source mapping")
            .position_at(0)
            .expect("physical resume position"),
    });
    let before = track.planned_end(rate).expect("forecast before source EOF");
    let mut metadata = vec![first.meta];
    loop {
        hang_tick!();
        let _ = source.prepare_deferred();
        source.finish_deferred();
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                source.admitted();
                metadata.push(data.meta);
                hang_reset!();
            }
            TrackStep::Eof | TrackStep::Produced(Fetch::NaturalEof) => break,
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Failed(failure) | TrackStep::Produced(Fetch::Failure { failure }) => {
                panic!("resumed generation failed: {failure:?}")
            }
        }
    }
    let final_span = metadata
        .last()
        .expect("physical tail")
        .source_span
        .expect("tail law");
    let (endpoint, endpoint_denominator) = final_span
        .source_ratio_at(final_span.output_frames())
        .expect("exact physical EOF");
    assert_eq!(final_span.sample_rate(), rate);
    assert_eq!(
        (endpoint, endpoint_denominator),
        (
            141_107,
            std::num::NonZeroU128::new(160).expect("retained endpoint phase")
        )
    );
    let endpoint_denominator =
        endpoint_denominator.get() * u128::from(final_span.sample_rate().get());
    let physical = metadata
        .iter()
        .find_map(|meta| {
            let span = meta.source_span.expect("physical source law");
            (0..=span.output_frames()).find_map(|offset| {
                let (position, denominator) = span
                    .source_ratio_at(offset)
                    .expect("exact source coordinate");
                let denominator = denominator.get() * u128::from(span.sample_rate().get());
                (position * endpoint_denominator >= endpoint * denominator).then(|| {
                    frame(i64::try_from(meta.lane_frame + offset).expect("physical session EOF"))
                })
            })
        })
        .expect("first physical EOF boundary");
    assert_eq!(
        physical,
        frame(3_524),
        "881 real remaining native PCM frames at quarter speed"
    );
    assert_eq!(source.media_end(), Some(Duration::from_millis(20)));
    track.duration = source
        .media_end()
        .zip(source.media_origin())
        .map(|(end, origin)| end.checked_sub(origin).expect("reconciled media extent"));
    let after = track.planned_end(rate).expect("forecast after source EOF");
    assert_eq!(
        (before, after),
        (Some(physical), Some(physical)),
        "SourceEnd frame 1/48000 must keep the same endpoint clock before and after EOF"
    );
}

#[kithara::test]
#[case::resume_without_pending_lane(false)]
#[case::resume_with_pending_lane(true)]
fn stopped_raw_mark_stays_raw_while_pause_and_resume_publish_media_position(
    #[case] pending_lane: bool,
) {
    let mut rig = rig();
    let (mut track, mut inbox) = loaded(&mut rig, A, "media-stop-resume");
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    );
    let mut started = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut started);
    track.media_origin = Some(Duration::from_millis(576));
    let stopped = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Pause {
            at: When::At(frame(128)),
        },
    )
    .expect("native Stop");
    let mut stopped_receipts = rig.block(frame(128), 2.576);
    let receipt = stopped_receipts
        .iter()
        .find(|receipt| receipt.seq == stopped)
        .expect("Stop owner receipt");
    assert!(matches!(receipt.outcome, Outcome::Applied { at, .. } if at == frame(128)));
    assert!(receipt.batch.commands.iter().any(|part| matches!(part,
        DeckPart::Returned(Returned::Stopped { slot: A, resume: Some(mark) })
            if mark.position == Duration::from_millis(2_576)
    )));
    settle(&mut track, &mut rig, &mut stopped_receipts);
    assert_eq!(
        track.resume.expect("raw pause resume").position,
        Duration::from_millis(2_576)
    );
    assert_eq!(track.snapshot().position, Duration::from_secs(2));
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Paused {
            at: Duration::from_secs(2)
        }
    );

    if pending_lane {
        rig.with_outbox(|out| {
            track.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.25), When::Next),
                out,
            )
        })
        .expect("live scope")
        .expect("pending lane speed");
    }
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::Play {
                at: When::At(frame(256)),
            },
            out,
        )
    })
    .expect("live scope")
    .expect("resume");
    let commands = lane_commands(&mut inbox);
    if pending_lane {
        assert!(commands.iter().any(|command| matches!(command,
            LaneCommand::Segment { from, .. } if *from == Duration::from_millis(2_576)
        )));
    } else {
        assert!(
            commands.is_empty(),
            "a ready paused segment resumes without a seek"
        );
        let mut resumed = rig.block(frame(256), 2.576);
        settle(&mut track, &mut rig, &mut resumed);
        assert_eq!(
            track.snapshot().status,
            TrackStatus::Playing { since: frame(256) }
        );
    }
    assert_eq!(track.snapshot().position, Duration::from_secs(2));

    if !pending_lane {
        let paused = command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Pause {
                at: When::At(frame(384)),
            },
        )
        .expect("second Stop");
        let mut paused_receipts = rig.block(frame(384), 2.576);
        let old_mark = paused_receipts
            .iter()
            .find(|receipt| receipt.seq == paused)
            .expect("second Stop owner receipt")
            .batch
            .commands
            .iter()
            .find_map(|part| match part {
                DeckPart::Returned(Returned::Stopped {
                    slot: A,
                    resume: Some(mark),
                }) => Some(*mark),
                _ => None,
            })
            .expect("renderer-owned stopped mark");
        settle(&mut track, &mut rig, &mut paused_receipts);
        assert_eq!(track.resume, Some(old_mark));

        let target = Duration::from_secs(10);
        let raw_target = Duration::from_millis(10_576);
        let output = mock::output(None).get();
        let deck = crate::DeckSnapshot {
            slots: vec![SlotSnapshot {
                state: SlotState::Stopped,
                mark: Some(old_mark),
                position: old_mark.position.as_secs_f64(),
                source: Some(AudioChunkInfo {
                    segment: old_mark.lane.segment,
                    media_origin: track.media_origin,
                    visible_origin: track.visible_origin,
                    media_end: track.duration,
                    ..AudioChunkInfo::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let sought = old_mark.lane.segment.next();
        let seek = {
            let fixture = &mut *rig;
            let mut scope = fixture.ring.scope(fixture.scope).expect("live scope");
            let mut out =
                Outbox::new(&mut scope, &mut fixture.dispatcher).in_pass(crate::DeckPass {
                    mix: DeckMixSettings::default(),
                    suspended: false,
                    now: frame(384),
                    delivery: FrameCount::new(0),
                    output: &output,
                    deck: &deck,
                });
            let seek = track
                .apply(TrackCommand::Seek { to: target }, &mut out)
                .expect("paused seek")
                .expect("seek adoption");
            assert_eq!(track.segment, sought);
            assert_eq!(track.ready, None);
            assert_eq!(
                track
                    .apply(
                        TrackCommand::Play {
                            at: When::At(frame(512)),
                        },
                        &mut out,
                    )
                    .expect("Play waits for the seek lane"),
                None
            );
            assert_eq!(track.segment, sought, "Play preserves the admitted seek");
            assert_eq!(track.mark, Some(old_mark));
            seek
        };
        inbox.drain();
        let due = inbox.next_due(old_mark.lane, 1).expect("seek lane command");
        assert!(matches!(due.commands(),
            [LaneCommand::Segment { id, from, .. }]
                if *id == sought && *from == raw_target
        ));
        due.apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(sought),
        });
        assert!(
            inbox.next_due(old_mark.lane, 1).is_none(),
            "immediate Play does not add a backwards segment"
        );
        rig.with_outbox(|out| track.tick(frame(512), out))
            .expect("ready seek tick");
        assert_eq!(track.snapshot().position, target);
        let mut receipts = rig.block(frame(512), raw_target.as_secs_f64());
        let settled = settle(&mut track, &mut rig, &mut receipts);
        assert!(
            settled
                .iter()
                .any(|settled| { matches!(settled, Settled::Applied { seq, .. } if *seq == seek) })
        );
        assert_eq!(track.segment, sought);
        assert_eq!(
            track.snapshot().status,
            TrackStatus::Playing { since: frame(512) }
        );
        assert_eq!(track.snapshot().position, target);
    }
}

#[kithara::test]
#[kithara::hang_watchdog(ctx = String)]
#[case::known_raw_mark(Some(2_576), 2_000, 2_576)]
#[case::commanded_media_without_a_raw_mark(None, 2_000, 2_576)]
#[case::untrimmed_prefix(Some(250), 0, 250)]
fn cancelled_parked_repeat_restores_the_raw_coordinate_without_reapplying_media_origin(
    #[case] mark_ms: Option<u64>,
    #[case] media_ms: u64,
    #[case] expected_raw_ms: u64,
) {
    let mut rig = rig();
    let (mut track, mut inbox) = loaded(&mut rig, A, "media-parked-repeat");
    rig.with_outbox(|out| track.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("live scope")
        .expect("initial Start");
    let mut started = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut started);
    let original = track.segment;
    let original_basis = track.source_basis.expect("original prepared input basis");
    track.media_origin = Some(Duration::from_millis(576));
    track.position = Duration::from_millis(media_ms);
    let mark = mark_ms.map(|raw_ms| SlotMark {
        session: frame(0),
        lane: LaneFrame {
            segment: original,
            frame: 0,
        },
        position: Duration::from_millis(raw_ms),
    });
    let output = mock::output(None).get();
    let deck = crate::DeckSnapshot {
        slots: vec![SlotSnapshot {
            state: SlotState::Playing,
            mark,
            position: mark.map_or(0.0, |mark| mark.position.as_secs_f64()),
            source: Some(AudioChunkInfo {
                segment: original,
                media_origin: track.media_origin,
                visible_origin: track.visible_origin,
                media_end: track.duration,
                ..AudioChunkInfo::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let repeated = {
        let fixture = &mut *rig;
        let mut scope = fixture.ring.scope(fixture.scope).expect("live scope");
        let mut out = Outbox::new(&mut scope, &mut fixture.dispatcher).in_pass(crate::DeckPass {
            mix: DeckMixSettings::default(),
            suspended: false,
            now: frame(0),
            delivery: FrameCount::new(0),
            output: &output,
            deck: &deck,
        });
        track
            .apply(TrackCommand::PlayAfter { track: A }, &mut out)
            .expect("repeat preparation")
            .expect("parked repeat")
    };
    assert_eq!(track.snapshot().mark, mark);
    assert_eq!(
        track.adopting.last().expect("repeat adoption").parked,
        Some((original, Duration::from_millis(expected_raw_ms)))
    );
    rig.with_outbox(|out| track.apply(TrackCommand::Supersede, out))
        .expect("live scope")
        .expect("cancel repeat");
    let mut refused = rig.block(frame(0), 0.0);
    assert!(refused.iter().any(|receipt| receipt.seq == repeated
        && matches!(receipt.outcome, Outcome::Rejected(Rejection::Stale))));
    settle(&mut track, &mut rig, &mut refused);
    assert_eq!(track.segment, original);
    inbox.drain();
    let mut restored = false;
    while let Some(due) = inbox.next_due(LaneFrame::default(), 1) {
        hang_tick!(format!("restoring raw repeat segment {original:?}"));
        restored |= due.commands().iter().any(|command| {
            matches!(command,
                LaneCommand::Segment { id, from, .. }
                    if *id == original && *from == Duration::from_millis(expected_raw_ms)
            )
        });
        due.apply(kithara_render::LaneApplied {
            source_basis: None,
            engine_latency: FrameCount::new(0),
            ready: Some(original),
        });
        hang_reset!();
    }
    assert!(
        restored,
        "the cancelled adoption restores its actual raw segment position"
    );
    track.settle_lane();
    assert_eq!(track.source_basis, Some(original_basis));
    assert_eq!(track.snapshot().position, Duration::from_millis(media_ms));
    assert_eq!(rig.mixer.held(A), Some("media-parked-repeat"));
}
