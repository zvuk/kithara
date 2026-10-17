use kithara_audio::AudioSource;
use kithara_command::{Outcome, Rejection, Seq};
use kithara_signal::{AudioChunkInfo, SourceSpan};

use super::*;
use crate::{
    LoadRefusal,
    dispatcher::LaneTask,
    worker::{PcmReceiver, node::fixture::prepared_node_with_info},
};

fn entry(segment: SegmentId, lane_frame: u64, source_frame: u64, revision: u64) -> AudioChunk {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("fixture rate"));
    let mut packet = chunk(spec, segment, 0, source_frame, &[0.5; 2_048]);
    packet.meta.lane_frame = lane_frame;
    packet.meta.render_revision = revision;
    packet.meta.source_span =
        SourceSpan::new(source_frame, source_frame + 1_024, spec.sample_rate, 1_024)
            .map(|span| span.with_render_revision(revision));
    packet
}

fn admit(node: &mut DecoderNode<ScriptedSource, TestPools>, packet: AudioChunk) {
    node.pending = Some(PendingPacket {
        packet: PcmPacket::Chunk(Box::new(packet)),
        source_end: None,
    });
    assert_eq!(node.tick(), TickResult::Progress);
}

#[kithara::test(tokio)]
async fn a_chunk_from_another_decode_epoch_proves_nothing() {
    let source = ScriptedSource::new((0..2).map(|index| {
        TrackStep::Produced(Fetch::data(entry(
            SegmentId::FIRST,
            0,
            44_100 + index * 1_024,
            0,
        )))
    }));
    let (mut node, _receiver, mut lane) = prepared_node(source, 4, 2).await;
    let stale = SegmentId::FIRST;
    let positioned = stale.next();
    node.pending = Some(PendingPacket {
        packet: PcmPacket::Chunk(Box::new(entry(stale, 0, 0, 0))),
        source_end: None,
    });
    let seq = lane
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::Segment {
                    id: positioned,
                    from: Duration::from_secs(1),
                    speed: kithara_sync::SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("positioned segment");
    node.synchronize().expect("synchronous positioning");
    node.pending = Some(PendingPacket {
        packet: PcmPacket::Chunk(Box::new(entry(stale, 0, 0, 0))),
        source_end: None,
    });
    node.synchronize().expect("retire stale decode output");
    assert!(
        lane.receipts().next().is_none(),
        "PCM decoded before the lane was positioned neither proves nor fails it"
    );
    assert!(matches!(node.preload_status(), Ok(false)));

    for (lane_frame, source_frame) in [(0, 44_100), (1_024, 45_124)] {
        let Fetch::Data { data, source_end } = next_rendered(&mut node) else {
            panic!("mapped source PCM");
        };
        assert_eq!(data.meta.segment, positioned);
        assert_eq!(data.meta.lane_frame, lane_frame);
        assert_eq!(
            data.meta.source_span.expect("source map").start(),
            source_frame
        );
        node.pending = Some(PendingPacket {
            packet: PcmPacket::Chunk(Box::new(data)),
            source_end,
        });
        assert_eq!(node.tick(), TickResult::Progress);
    }
    let receipt = lane.receipts().next().expect("ready verdict");
    assert_eq!(receipt.seq(), seq);
    assert!(matches!(
        receipt.outcome(),
        Outcome::Applied { data, .. } if data.ready == Some(positioned)
    ));
    assert!(matches!(node.preload_status(), Ok(true)));
    fractional_segment_entry().await;
    for control_before_output in [false, true] {
        entry_under_backpressure(control_before_output).await;
    }
    for (target, landing, first_frame, rate, visible_origin, accepts_entry) in [
        (
            Duration::from_secs(1),
            Duration::from_millis(900),
            44_100,
            44_100,
            Some(Duration::ZERO),
            true,
        ),
        (
            Duration::ZERO,
            Duration::ZERO,
            480,
            48_000,
            Some(Duration::from_millis(10)),
            true,
        ),
        (
            Duration::from_millis(15),
            Duration::ZERO,
            720,
            48_000,
            Some(Duration::from_millis(10)),
            true,
        ),
        (Duration::ZERO, Duration::ZERO, 480, 48_000, None, false),
        (
            Duration::from_millis(15),
            Duration::ZERO,
            480,
            48_000,
            Some(Duration::from_millis(10)),
            true,
        ),
    ] {
        post_trim_entry(
            target,
            landing,
            first_frame,
            rate,
            visible_origin,
            accepts_entry,
        )
        .await;
    }
}

#[kithara::test(tokio)]
#[case::before_the_origin(-1, 0)]
#[case::after_the_origin(1, 0)]
#[case::from_another_mapping(0, 1)]
async fn readiness_needs_the_exact_origin_and_revision_of_the_activation(
    #[case] offset: i64,
    #[case] revision_shift: u64,
) {
    let origin = 44_100_u64;
    let source = ScriptedSource::new([TrackStep::Produced(Fetch::data(entry(
        SegmentId::FIRST,
        0,
        origin,
        0,
    )))]);
    let (mut node, mut receiver, mut lane) = prepared_node(source, 4, 2).await;
    let positioned = SegmentId::FIRST.next();
    assert_ne!(origin, 0, "the fixture activates mid-recording");
    let seq = lane
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::Segment {
                    id: positioned,
                    from: Duration::from_secs(1),
                    speed: kithara_sync::SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("activation command");
    node.synchronize().expect("synchronous positioning");
    let Fetch::Data {
        mut data,
        source_end,
    } = next_rendered(&mut node)
    else {
        panic!("real mapped entry output");
    };
    let span = data.meta.source_span.expect("real entry mapping");
    assert_eq!(span.start(), origin);
    assert_eq!(data.meta.render_revision, seq.get());
    let wrong_revision = seq.get() + revision_shift;
    data.meta.render_revision = wrong_revision;
    data.meta.source_span = SourceSpan::new(
        span.start()
            .checked_add_signed(offset)
            .expect("shifted origin"),
        span.end()
            .checked_add_signed(offset)
            .expect("shifted endpoint"),
        span.sample_rate(),
        span.output_frames(),
    )
    .map(|span| span.with_render_revision(wrong_revision));
    node.pending = Some(PendingPacket {
        packet: PcmPacket::Chunk(Box::new(data)),
        source_end,
    });
    assert_eq!(node.tick(), TickResult::Progress);
    let receipt = lane.receipts().next().expect("readiness verdict");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Refused(TrackFailureKind::Render))
        ),
        "a lane that does not enter at {origin} under revision {} cannot present the plan",
        seq.get()
    );
    assert!(receiver.pop().is_none(), "invalid PCM was never published");
    assert!(!node.source.is_preloaded());
    assert!(matches!(
        node.preload_status(),
        Err(LoadRefusal::Source(TrackFailureKind::Render))
    ));
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(matches!(
        receiver.pop(),
        Some(PcmPacket::Failed {
            failure: TrackFailureKind::Render,
            ..
        })
    ));
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(receiver.pop().is_none());
    assert!(lane.receipts().next().is_none());
}

#[kithara::hang_watchdog]
fn next_rendered(node: &mut DecoderNode<ScriptedSource, TestPools>) -> Fetch<AudioChunk> {
    loop {
        hang_tick!();
        let _ = node.source.prepare_deferred();
        node.source.finish_deferred();
        match node.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, source_end }) => {
                hang_reset!();
                return Fetch::Data { data, source_end };
            }
            TrackStep::StateChanged => {}
            _ => panic!("the scripted source must produce its next mapped PCM"),
        }
    }
}

async fn entry_under_backpressure(control_before_output: bool) {
    let origin = 48_510;
    let visible_origin = Duration::from_millis(1_100);
    let mut source = ScriptedSource::new((0..4).map(|index| {
        let mut packet = entry(SegmentId::FIRST, 0, origin + index * 1_024, 0);
        packet.meta.media_origin = Some(Duration::ZERO);
        packet.meta.visible_origin = Some(visible_origin);
        TrackStep::Produced(Fetch::data(packet))
    }));
    source.landing = Some(visible_origin);
    let (mut node, mut receiver, mut lane) = prepared_node(source, 1, 2).await;
    admit(&mut node, entry(SegmentId::FIRST, 0, 0, 0));
    let positioned = SegmentId::FIRST.next();
    let seq = lane
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::Segment {
                    id: positioned,
                    from: Duration::from_secs(1),
                    speed: kithara_sync::SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("coarse-landing segment");
    node.synchronize().expect("actual coarse source landing");
    let mut changed = None;
    if control_before_output {
        changed = Some(
            lane.send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::SetSpeed(kithara_sync::SpeedCurve::Constant(
                        1.0,
                    ))],
                },
            )
            .expect("entry law replacement"),
        );
        node.synchronize().expect("replace law before production");
        let control = lane.receipts().next().expect("actual control receipt");
        assert_eq!(control.seq(), changed.expect("actual control sequence"));
        assert!(matches!(control.outcome(), Outcome::Applied { data, .. } if data.ready.is_none()));
    }
    let Fetch::Data { data, source_end } = next_rendered(&mut node) else {
        panic!("mapped entry PCM");
    };
    let first_revision = changed.unwrap_or(seq).get();
    assert_eq!(data.meta.render_revision, first_revision);
    assert_eq!(data.meta.source_span.expect("entry map").start(), origin);
    assert_eq!(data.meta.lane_frame, 0);
    assert_eq!(node.source.cursor().frame, 1_024);
    node.pending = Some(PendingPacket {
        packet: PcmPacket::Chunk(Box::new(data)),
        source_end,
    });
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(lane.receipts().next().is_none());
    assert!(!node.source.is_preloaded());
    if !control_before_output {
        changed = Some(
            lane.send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::SetSpeed(kithara_sync::SpeedCurve::Constant(
                        1.0,
                    ))],
                },
            )
            .expect("law after the pending entry"),
        );
        node.synchronize()
            .expect("later law leaves pending entry intact");
        let control = lane.receipts().next().expect("actual control receipt");
        assert_eq!(control.seq(), changed.expect("actual control sequence"));
        assert!(matches!(control.outcome(), Outcome::Applied { data, .. } if data.ready.is_none()));
    }
    let changed = changed.expect("actual control sequence");
    assert!(lane.receipts().next().is_none());
    assert!(receiver.pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    let Some(PcmPacket::Chunk(first)) = receiver.pop() else {
        panic!("entry packet admitted after backpressure");
    };
    assert_eq!(first.meta.render_revision, first_revision);
    assert!(!node.source.is_preloaded());
    assert!(lane.receipts().next().is_none());
    let Fetch::Data { data, source_end } = next_rendered(&mut node) else {
        panic!("mapped successor PCM");
    };
    assert_eq!(data.meta.render_revision, changed.get());
    assert_eq!(data.meta.lane_frame, 1_024);
    node.pending = Some(PendingPacket {
        packet: PcmPacket::Chunk(Box::new(data)),
        source_end,
    });
    assert_eq!(node.tick(), TickResult::Progress);
    let receipt = lane
        .receipts()
        .next()
        .expect("ready only after two admitted packets");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(positioned))
    );
    assert!(receiver.pop().is_some());
    assert_host_rate_entry(&mut node, &mut receiver, &mut lane, positioned, changed);
}

fn assert_host_rate_entry(
    node: &mut DecoderNode<ScriptedSource, TestPools>,
    receiver: &mut PcmReceiver,
    lane: &mut Sender<LaneProtocol>,
    positioned: SegmentId,
    changed: Seq,
) {
    let next = positioned.next();
    let rate_seq = lane
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::SetHostRate {
                    id: next,
                    rate: NonZeroU32::new(48_000).expect("new host rate"),
                }],
            },
        )
        .expect("host rate segment");
    node.synchronize()
        .expect("host rate keeps its actual speed law");
    for _ in 0..2 {
        let Fetch::Data { data, source_end } = next_rendered(node) else {
            panic!("mapped host-rate PCM");
        };
        assert_eq!(data.meta.segment, next);
        assert_eq!(data.meta.render_revision, changed.get());
        node.pending = Some(PendingPacket {
            packet: PcmPacket::Chunk(Box::new(data)),
            source_end,
        });
        assert_eq!(node.tick(), TickResult::Progress);
        assert!(receiver.pop().is_some());
    }
    let receipt = lane.receipts().next().expect("host-rate readiness");
    assert_eq!(receipt.seq(), rate_seq);
    assert!(matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(next)));
    assert!(lane.receipts().next().is_none());
}

fn nanosecond_projected_entry(meta: AudioChunkInfo, target: Duration) -> Option<AudioChunkInfo> {
    let span = meta.source_span.expect("exact raw entry span");
    let exact_entry = span.source_ratio_at(0).expect("actual source entry");
    let duration_entry = (
        target
            .as_nanos()
            .checked_mul(u128::from(meta.spec.sample_rate.get()))
            .expect("duration source coordinate"),
        std::num::NonZeroU128::new(1_000_000_000).expect("nanosecond source clock"),
    );
    let projected = span
        .scaled_from(exact_entry, duration_entry, (1, std::num::NonZeroU128::MIN))
        .expect("duration-projected entry geometry");
    if projected.source_ratio_at(0) == Some(exact_entry) {
        return None;
    }
    assert_eq!(projected.position_at(0), Some(target));
    assert_eq!(projected.render_revision(), span.render_revision());
    assert_eq!(projected.mapping_revision(), span.mapping_revision());
    Some(AudioChunkInfo {
        source_span: Some(projected),
        ..meta
    })
}

async fn fractional_segment_entry() {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("fractional entry rate"));
    for (first_frame, target, visible_origin, expected_sample, expected_ratio, rejected_frame) in [
        (
            769_202,
            Duration::from_nanos(16_025_056_689),
            None,
            0.721072_f32,
            (48_075_170_067, 62_500),
            769_202,
        ),
        (
            1_104,
            Duration::from_nanos(23_020_833),
            Some(Duration::from_nanos(23_020_833)),
            0.999984_f32,
            (69_062_499, 62_500),
            1_104,
        ),
        (
            1_105,
            Duration::from_nanos(23_020_833),
            Some(Duration::from_nanos(23_020_833)),
            0.0_f32,
            (1_105, 1),
            1_104,
        ),
        (
            1_105,
            Duration::from_nanos(23_020_833),
            None,
            0.0_f32,
            (1_105, 1),
            1_104,
        ),
    ] {
        for source_rate in [48_000, 44_100] {
            let samples = (0_u16..4_096)
                .flat_map(|frame| [f32::from(frame), -f32::from(frame)])
                .collect::<Vec<_>>();
            let mut packet = chunk(spec, SegmentId::FIRST, 0, first_frame, &samples);
            packet.meta.visible_origin = visible_origin;
            let source_rate = NonZeroU32::new(source_rate).expect("decoded source rate");
            packet.meta.source_span = Some(
                SourceSpan::try_from((
                    u128::from(first_frame) * u128::from(source_rate.get()),
                    u128::from(source_rate.get()),
                    std::num::NonZeroU128::from(spec.sample_rate),
                    source_rate,
                    u64::from(packet.meta.frames),
                ))
                .expect("exact decoded source geometry"),
            );
            let native = packet.meta;
            let source = ScriptedSource::new([TrackStep::Produced(Fetch::data(packet))]);
            let (mut node, mut receiver, mut lane) = prepared_node_with_info(
                source,
                4,
                1,
                AudioChunkInfo {
                    spec,
                    ..AudioChunkInfo::default()
                },
            )
            .await;
            let positioned = SegmentId::FIRST.next();
            let seq = lane
                .send(
                    When::Next,
                    Batch {
                        basis: Vec::new(),
                        commands: vec![LaneCommand::Segment {
                            id: positioned,
                            from: target,
                            speed: kithara_sync::SpeedCurve::Constant(1.0),
                        }],
                    },
                )
                .expect("fractional segment");
            node.synchronize().expect("prefix seek admitted");
            let Fetch::Data { data, source_end } = next_rendered(&mut node) else {
                panic!("fractional physical output");
            };
            assert_eq!(data.samples[0], expected_sample);
            assert_eq!(data.samples[1], -expected_sample);
            let span = data.meta.source_span.expect("exact raw entry span");
            assert_eq!(span.position_at(0), Some(target));
            assert_eq!(
                span.source_ratio_at(0),
                Some((
                    expected_ratio.0,
                    std::num::NonZeroU128::new(expected_ratio.1).expect("raw ratio")
                )),
            );
            let mut retained_prefix = data.meta;
            retained_prefix.source_span = SourceSpan::new(
                rejected_frame,
                rejected_frame + span.output_frames(),
                spec.sample_rate,
                span.output_frames(),
            )
            .map(|span| {
                span.with_render_revision(data.meta.render_revision)
                    .with_mapping_revision(data.meta.mapping_revision)
            });
            assert_eq!(
                node.source.check_admission(&retained_prefix),
                Err(TrackFailureKind::Render),
                "retained prefix PCM cannot present the later requested entry"
            );
            if let Some(projected_entry) = nanosecond_projected_entry(data.meta, target) {
                assert_eq!(
                    node.source.check_admission(&projected_entry),
                    Err(TrackFailureKind::Render),
                    "equal nanosecond positions cannot replace the exact source entry"
                );
            }
            assert_eq!(data.meta.segment, positioned);
            assert_eq!(data.meta.lane_frame, 0);
            node.pending = Some(PendingPacket {
                packet: PcmPacket::Chunk(Box::new(data)),
                source_end,
            });
            assert_eq!(node.tick(), TickResult::Progress);
            let receipt = lane
                .receipts()
                .next()
                .expect("actual fractional entry ready");
            assert_eq!(receipt.seq(), seq);
            assert!(matches!(
                receipt.outcome(),
                Outcome::Applied { data, .. }
                    if data.ready == Some(positioned) && data.source_basis == Some(native)
            ));
            assert!(receiver.pop().is_some());
            assert!(lane.receipts().next().is_none());
        }
    }
}

async fn post_trim_entry(
    target: Duration,
    landing: Duration,
    first_frame: u64,
    rate: u32,
    visible_origin: Option<Duration>,
    accepts_entry: bool,
) {
    let spec = AudioSpec::new(2, NonZeroU32::new(rate).expect("post-trim source rate"));
    let mut source = ScriptedSource::new((0..2).map(|index| {
        let frame = first_frame + index * 1_024;
        let mut packet = chunk(spec, SegmentId::FIRST, 0, frame, &[0.5; 2_048]);
        packet.meta.media_origin = Some(Duration::ZERO);
        packet.meta.visible_origin = visible_origin;
        TrackStep::Produced(Fetch::data(packet))
    }));
    source.landing = Some(landing);
    let (mut node, mut receiver, mut lane) = prepared_node_with_info(
        source,
        4,
        2,
        AudioChunkInfo {
            spec,
            ..AudioChunkInfo::default()
        },
    )
    .await;
    let positioned = SegmentId::FIRST.next();
    let seq = lane
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::Segment {
                    id: positioned,
                    from: target,
                    speed: kithara_sync::SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("seek before post-trim PCM");
    node.synchronize().expect("raw seek landing");
    if !accepts_entry {
        assert!(matches!(
            node.source.step_track(),
            TrackStep::Failed(TrackFailureKind::Render)
        ));
        assert!(
            receiver.pop().is_none(),
            "missed entry PCM was never published"
        );
        assert_eq!(node.tick(), TickResult::Progress);
        let receipt = lane.receipts().next().expect("missed entry verdict");
        assert_eq!(receipt.seq(), seq);
        assert!(matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Refused(TrackFailureKind::Render))
        ));
        assert!(!node.source.is_preloaded());
        assert!(matches!(
            node.preload_status(),
            Err(LoadRefusal::Source(TrackFailureKind::Render))
        ));
        assert!(matches!(
            receiver.pop(),
            Some(PcmPacket::Failed {
                segment,
                failure: TrackFailureKind::Render,
            }) if segment == positioned
        ));
        assert_eq!(node.tick(), TickResult::Backpressured);
        assert!(receiver.pop().is_none());
        assert!(lane.receipts().next().is_none());
        assert!(!node.source.is_preloaded());
        return;
    }
    let expected_entry = visible_origin.map_or(target, |origin| origin.max(target));
    let mut expected_source_frame = spec.frame_at(expected_entry).expect("entry frame");
    for index in 0..2 {
        let Fetch::Data { data, source_end } = next_rendered(&mut node) else {
            panic!("post-trim mapped output");
        };
        let span = data.meta.source_span.expect("post-trim mapping");
        assert_eq!(span.sample_rate(), spec.sample_rate);
        assert_eq!(span.start(), expected_source_frame);
        expected_source_frame = span.end();
        assert_eq!(data.meta.render_revision, seq.get());
        node.pending = Some(PendingPacket {
            packet: PcmPacket::Chunk(Box::new(data)),
            source_end,
        });
        assert_eq!(node.tick(), TickResult::Progress);
        assert!(receiver.pop().is_some());
        assert_eq!(node.source.is_preloaded(), index == 1);
        if index == 0 {
            assert!(lane.receipts().next().is_none());
        }
    }
    let receipt = lane.receipts().next().expect("post-trim readiness");
    assert_eq!(receipt.seq(), seq);
    assert!(
        matches!(receipt.outcome(), Outcome::Applied { data, .. } if data.ready == Some(positioned))
    );
    assert!(lane.receipts().next().is_none());
}
