pub(super) use std::num::{NonZeroU32, NonZeroUsize};

pub(super) use kithara_audio::{Fetch, SourceEnd, TrackStep, WaitingReason};
pub(super) use kithara_command::{Batch, Sender, When};
pub(super) use kithara_platform::{sync::Arc, time::Duration};
pub(super) use kithara_signal::{AudioChunk, AudioSpec, SegmentId};
pub(super) use kithara_test_fixtures::unit_fixtures::eq_silence as node_silence;
pub(super) use kithara_test_utils::kithara;
pub(super) use kithara_worker::{Task, TickResult};

use super::*;
use crate::dispatcher::LaneTask;
pub(super) use crate::{
    LaneCommand, LaneProtocol,
    mock::prepared_node,
    test_pools::{Pools, TestPools, pools},
    worker::{EngineLoad, packet_tests::chunk},
};

pub(in crate::worker) fn empty_chunk(_pools: &Pools) -> AudioChunk {
    chunk(
        AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate")),
        SegmentId::FIRST,
        0,
        0,
        &[0.0, 0.0],
    )
}

pub(super) use crate::mock::ScriptedSource;

pub(super) fn produced() -> TrackStep<AudioChunk> {
    TrackStep::Produced(Fetch::data(empty_chunk(&pools())))
}
/// Where [`segment`] positions the source.
pub(in crate::worker) const SEGMENT_FROM: Duration = Duration::from_secs(1);
/// The first chunk a source produces once it landed at `at`.
fn produced_at(at: Duration) -> TrackStep<AudioChunk> {
    let spec = empty_chunk(&pools()).spec();
    let frame = spec.frame_at(at).expect("representable landing");
    TrackStep::Produced(Fetch::data(chunk(
        spec,
        SegmentId::FIRST,
        0,
        frame,
        &[0.0, 0.0],
    )))
}
pub(in crate::worker) fn segment(lane: &mut Sender<LaneProtocol>) -> SegmentId {
    let id = SegmentId::FIRST.next();
    lane.send(
        When::Next,
        Batch {
            basis: Vec::new(),
            commands: vec![LaneCommand::Segment {
                id,
                from: SEGMENT_FROM,
                speed: kithara_sync::SpeedCurve::Constant(1.0),
            }],
        },
    )
    .expect("segment batch");
    id
}

#[kithara::test(tokio)]
async fn transient_upstream_wait_does_not_complete_preload() {
    let (mut node, _receiver, _lane) = prepared_node(
        ScriptedSource::new([
            TrackStep::StateChanged,
            TrackStep::Blocked(WaitingReason::Waiting),
        ]),
        4,
        3,
    )
    .await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(matches!(node.preload_status(), Ok(false)));
    assert_eq!(node.tick(), TickResult::Waiting);
    assert!(matches!(node.preload_status(), Ok(false)));
}

#[kithara::test(tokio)]
async fn worker_preload_gate_fires_on_failure() {
    let failure = TrackFailureKind::Decode {
        kind: kithara_audio::DecodeErrorKind::InvalidData,
    };
    let (mut node, _receiver, _lane) =
        prepared_node(ScriptedSource::new([TrackStep::Failed(failure)]), 32, 8).await;
    let result = kithara_platform::time::timeout(Duration::from_secs(1), preload(&mut node))
        .await
        .expect("decoder failure must complete the preload wait");
    assert_eq!(result, Err(failure));
    assert_eq!(node.terminal, Some(Err(failure)));
}

#[kithara::test(tokio)]
async fn worker_telemetry_throttles_immediate_repeats() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let packet = chunk(spec, SegmentId::FIRST, 0, 0, &vec![0.0; 30_870]);
    let (mut node, mut receiver, _lane) = prepared_node(
        ScriptedSource::new([TrackStep::Produced(Fetch::data(packet))]),
        4,
        1,
    )
    .await;
    let meter = Arc::new(EngineLoad::default());
    meter.record(Duration::from_millis(5), 4_410, 44_100);
    node.engine_load = Some(Arc::clone(&meter));
    receiver.set_position(Duration::from_millis(100));
    assert_eq!(node.tick(), TickResult::Progress);
    let first = meter.snapshot();
    for _ in 0..2 {
        assert_eq!(
            receiver
                .decoded_frontier()
                .saturating_sub(receiver.position()),
            Duration::from_millis(250)
        );
        assert_eq!(receiver.decoded_frontier(), Duration::from_millis(350));
        assert!(meter.snapshot().is_active());
        assert_eq!(meter.snapshot().load(), first.load());
        assert_eq!(meter.snapshot().ms(), first.ms());
    }
    assert!(receiver.pop().is_some());
    assert!(
        receiver.pop().is_none(),
        "second immediate observation does not republish"
    );
}

#[kithara::test(tokio)]
async fn deferred_eof_event_keeps_the_decode_epoch() {
    let (mut node, mut receiver, mut lane) = prepared_node(ScriptedSource::new([]), 1, 1).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Progress);
    let live_segment = segment(&mut lane);
    assert_eq!(
        live_segment,
        SegmentId::FIRST.next(),
        "seek overtakes the deferred EOF read"
    );
    let mut eof_segments =
        std::iter::from_fn(|| receiver.pop()).filter_map(|packet| match packet {
            PcmPacket::Chunk(chunk) if chunk.meta.end_of_track => Some(chunk.meta.segment),
            _ => None,
        });
    assert_eq!(eof_segments.next(), Some(SegmentId::FIRST));
    assert_eq!(eof_segments.next(), None);
}

#[kithara::test(tokio)]
#[case(WaitingReason::Waiting)]
#[case(WaitingReason::WaitingDemand)]
#[case(WaitingReason::WaitingMetadata)]
async fn decoder_node_upstream_park_after_audio_opens_the_preload_gate(
    #[case] reason: WaitingReason,
) {
    let source = ScriptedSource::new([produced(), TrackStep::Blocked(reason)]);
    let (mut node, _receiver, _lane) = prepared_node(source, 4, 2).await;
    let _ = node.tick();
    assert!(
        !node.source.is_preloaded(),
        "the chunk quota is not met after one chunk"
    );
    let _ = node.tick();
    assert!(
        node.source.is_preloaded(),
        "a producer parked on {reason:?} with audio behind it must not strand the construction wait"
    );
}

#[kithara::test(tokio)]
async fn decoder_node_eof_under_backpressure() {
    let (mut node, mut receiver, _lane) =
        prepared_node(ScriptedSource::new([produced()]), 1, 1).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(node.terminal.is_none());
    assert!(
        node.pending.is_some(),
        "EOF waits for admission behind the full port"
    );
    assert!(matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if !packet.meta.end_of_track));
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.terminal.is_some());
    assert!(matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if packet.meta.end_of_track));
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(
        receiver.pop().is_none(),
        "current-segment EOF publishes exactly once"
    );
}

#[kithara::test(tokio)]
async fn decoder_node_does_not_republish_exhausted_warp_source_eof() {
    let (mut node, mut receiver, _lane) = prepared_node(ScriptedSource::new([]), 1, 1).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if packet.meta.end_of_track));
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(receiver.pop().is_none(), "one terminal packet");

    use kithara_audio::{SourceDiscontinuity, mock::AudioSourceMock};
    use kithara_signal::{AudioChunkInfo, SourceSpan};
    use unimock::{MockFn, Unimock};

    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("source rate"));
    let mapping = std::num::NonZeroU64::new(3);
    for (old_segment, old_revision) in [
        (SegmentId::FIRST, 7),
        (SegmentId::FIRST, 6),
        (SegmentId::FIRST.next(), 7),
    ] {
        let source = Unimock::new((
            AudioSourceMock::discontinuity
                .each_call(unimock::matching!())
                .returns(Some(SourceDiscontinuity::new(7, spec))),
            AudioSourceMock::media_origin
                .each_call(unimock::matching!())
                .returns(Some(Duration::from_millis(500))),
            AudioSourceMock::visible_origin
                .each_call(unimock::matching!())
                .returns(None),
            AudioSourceMock::media_end
                .each_call(unimock::matching!())
                .returns(Some(Duration::from_secs(2))),
            AudioSourceMock::step_track
                .some_call(unimock::matching!())
                .returns(TrackStep::Eof)
                .once(),
        ));
        let initial = AudioChunkInfo {
            spec,
            segment: old_segment,
            source_revision: old_revision,
            source_span: SourceSpan::new(5, 10, spec.sample_rate, 5)
                .map(|span| span.with_mapping_revision(mapping)),
            mapping_revision: mapping,
            source_byte_offset: Some(42),
            segment_index: Some(13),
            variant_index: Some(2),
            ..AudioChunkInfo::default()
        };
        let (mut node, mut receiver, _lane) =
            fixture::prepared_node_with_info(source, 1, 1, initial).await;
        assert_eq!(node.tick(), TickResult::Progress);
        assert_eq!(node.tick(), TickResult::Progress);
        let Some(PcmPacket::Chunk(terminal)) = receiver.pop() else {
            panic!("natural terminal packet")
        };
        assert!(terminal.meta.end_of_track);
        assert_eq!(terminal.meta.segment, SegmentId::FIRST);
        assert_eq!(terminal.meta.source_revision, 7);
        assert_eq!(terminal.meta.media_origin, Some(Duration::from_millis(500)));
        assert_eq!(terminal.meta.visible_origin, None);
        assert_eq!(terminal.meta.media_end, Some(Duration::from_secs(2)));
        let current = old_segment == SegmentId::FIRST && old_revision == 7;
        assert_eq!(
            terminal.meta.mapping_revision,
            current.then_some(mapping).flatten()
        );
        assert_eq!(terminal.meta.source_span.is_some(), current);
        assert_eq!(terminal.meta.source_byte_offset, current.then_some(42));
        assert_eq!(terminal.meta.segment_index, current.then_some(13));
        assert_eq!(terminal.meta.variant_index, current.then_some(2));
        assert_eq!(node.tick(), TickResult::Backpressured);
        assert!(receiver.pop().is_none());
    }
}

#[kithara::test(tokio)]
async fn decoder_node_records_engine_load_on_produced(node_silence: Vec<f32>) {
    let meter = Arc::new(EngineLoad::default());
    assert!(!meter.snapshot().is_active(), "idle before any tick");
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let packet = chunk(spec, SegmentId::FIRST, 0, 0, &node_silence[..8_820]);
    let (mut node, _receiver, _lane) = prepared_node(
        ScriptedSource::new([TrackStep::Produced(Fetch::data(packet))]),
        4,
        1,
    )
    .await;
    node.engine_load = Some(Arc::clone(&meter));
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(meter.snapshot().is_active(), "meter records Produced ticks");
}

#[kithara::test(tokio)]
async fn worker_observation_reads_do_not_republish_packets() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let packet = chunk(spec, SegmentId::FIRST, 0, 0, &vec![0.0; 30_870]);
    let (mut node, mut receiver, _lane) = prepared_node(
        ScriptedSource::new([TrackStep::Produced(Fetch::data(packet))]),
        4,
        1,
    )
    .await;
    let meter = Arc::new(EngineLoad::default());
    meter.record(Duration::from_millis(5), 4_410, 44_100);
    node.engine_load = Some(Arc::clone(&meter));
    receiver.set_position(Duration::from_millis(100));
    assert_eq!(node.tick(), TickResult::Progress);
    for _ in 0..2 {
        assert_eq!(
            receiver
                .decoded_frontier()
                .saturating_sub(receiver.position()),
            Duration::from_millis(250)
        );
        assert_eq!(receiver.decoded_frontier(), Duration::from_millis(350));
        assert!(meter.snapshot().is_active());
    }
    assert!(receiver.pop().is_some());
    assert!(
        receiver.pop().is_none(),
        "observations do not duplicate output"
    );
}

#[kithara::test(tokio)]
async fn decoder_node_distinguishes_failed_from_eof_on_the_wire() {
    let (mut eof_node, mut eof_receiver, _lane) =
        prepared_node(ScriptedSource::new([]), 1, 1).await;
    assert_eq!(eof_node.tick(), TickResult::Progress);
    assert_eq!(eof_node.tick(), TickResult::Progress);
    let eof_marker = eof_receiver.pop();
    let failed = TrackStep::Failed(TrackFailureKind::Decode {
        kind: kithara_audio::DecodeErrorKind::InvalidData,
    });
    let (mut failed_node, mut failed_receiver, _lane) =
        prepared_node(ScriptedSource::new([failed]), 1, 1).await;
    assert_eq!(failed_node.tick(), TickResult::Progress);
    let failed_marker = failed_receiver.pop();
    assert!(matches!(eof_marker, Some(PcmPacket::Chunk(packet)) if packet.meta.end_of_track));
    assert!(matches!(failed_marker, Some(PcmPacket::Failed { .. })));
}

#[kithara::test(tokio)]
async fn an_admitted_eof_keeps_its_segment_after_a_later_segment_opens() {
    let (mut node, mut receiver, mut lane) = prepared_node(ScriptedSource::new([]), 1, 1).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Progress);
    let next = segment(&mut lane);
    node.synchronize().expect("segment opens");
    assert_eq!(next, SegmentId::FIRST.next());
    assert_eq!(node.source.cursor().segment, next);
    let Some(PcmPacket::Chunk(eof)) = receiver.pop() else {
        panic!("EOF packet");
    };
    assert!(eof.meta.end_of_track);
    assert_eq!(eof.meta.segment, SegmentId::FIRST);
    assert!(receiver.pop().is_none());
}

#[kithara::test(tokio)]
async fn decoded_frontier_advances_only_after_final_port_admission() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let packet = chunk(spec, SegmentId::FIRST, 1, 1, &vec![0.0; 66_148]);
    let source = ScriptedSource::new([produced(), TrackStep::Produced(Fetch::data(packet))]);
    let (mut node, mut receiver, _lane) = prepared_node(source, 1, 1).await;
    let initial = spec.duration_for(1).expect("initial frame");
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    let prepared = node
        .pending
        .as_ref()
        .expect("one packet prepared behind the full port");
    let PcmPacket::Chunk(prepared) = &prepared.packet else {
        panic!("prepared PCM packet");
    };
    let prepared = prepared.meta;
    for _ in 0..3 {
        assert_eq!(node.tick(), TickResult::Backpressured);
        let pending = node
            .pending
            .as_ref()
            .expect("the same packet remains pending");
        assert!(matches!(&pending.packet, PcmPacket::Chunk(chunk) if chunk.meta == prepared));
        assert_eq!(receiver.decoded_frontier(), initial);
    }
    assert!(receiver.pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(receiver.decoded_frontier(), Duration::from_millis(750));
}

#[kithara::test(tokio)]
async fn a_pending_packet_stops_at_a_scheduled_speed_boundary() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let second = chunk(spec, SegmentId::FIRST, 0, 1, &[0.0; 8]);
    let source = ScriptedSource::new([produced(), TrackStep::Produced(Fetch::data(second))]);
    let (mut node, mut receiver, mut lane) = prepared_node(source, 1, 1).await;
    assert_eq!(node.tick(), TickResult::Progress);
    let at = crate::LaneFrame {
        segment: SegmentId::FIRST,
        frame: 3,
    };
    let seq = lane
        .send(
            When::At(at),
            Batch {
                basis: Vec::new(),
                commands: vec![LaneCommand::SetSpeed(kithara_sync::SpeedCurve::Constant(
                    1.0,
                ))],
            },
        )
        .expect("scheduled speed batch");
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(
        node.source.cursor(),
        at,
        "preparation stops at the queued command"
    );
    let pending = node
        .pending
        .as_ref()
        .expect("old-law prefix is prepared behind the full port");
    let PcmPacket::Chunk(prefix) = &pending.packet else {
        panic!("prepared PCM prefix")
    };
    assert_eq!(prefix.meta.lane_frame, 1);
    assert_eq!(prefix.frames(), 2);
    assert_eq!(prefix.meta.render_revision, 0);
    let prefix = prefix.meta;
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(node.source.cursor(), at);
    assert!(
        matches!(node.pending.as_ref().map(|pending| &pending.packet),
        Some(PcmPacket::Chunk(chunk)) if chunk.meta == prefix),
        "applying the next law retains the immutable old-law prefix"
    );
    assert!(receiver.pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    let Some(PcmPacket::Chunk(admitted)) = receiver.pop() else {
        panic!("admitted prefix")
    };
    assert_eq!(admitted.meta, prefix);
    assert_eq!(node.tick(), TickResult::Progress);
    let Some(PcmPacket::Chunk(next)) = receiver.pop() else {
        panic!("new-law suffix")
    };
    assert_eq!(next.meta.lane_frame, at.frame);
    assert_eq!(next.frames(), 2);
    assert_eq!(next.meta.render_revision, seq.get());
}

#[kithara::test(tokio)]
async fn source_end_commits_only_after_final_port_admission() {
    let end = SourceEnd::new(12_345, NonZeroU32::new(44_100).expect("rate"));
    let spec = AudioSpec::new(2, end.sample_rate());
    let packet = chunk(spec, SegmentId::FIRST, 0, 12_344, &[0.0; 2]);
    let source = ScriptedSource::new([TrackStep::Produced(Fetch::rendered(packet, end))]);
    let commits = Arc::clone(&source.commits);
    let (mut node, mut receiver, _lane) = prepared_node(source, 1, 1).await;
    node.pending = Some(PendingPacket {
        packet: PcmPacket::Chunk(Box::new(empty_chunk(&pools()))),
        source_end: None,
    });
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(
        node.pending.is_some(),
        "source output is prepared while the final port is full"
    );
    assert!(commits.lock().is_empty());
    assert!(receiver.pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(commits.lock().as_slice(), &[end]);
    assert_eq!(
        receiver.position(),
        Duration::ZERO,
        "admitting a source endpoint does not consume its PCM"
    );
}

#[kithara::test(tokio)]
async fn decoder_node_live_upstream_demand_does_not_tick_hang_wait() {
    let source = ScriptedSource::new([TrackStep::Blocked(WaitingReason::WaitingDemand)]);
    let (mut node, _receiver, _lane) = prepared_node(source, 2, 1).await;
    assert_eq!(node.tick(), TickResult::UpstreamPending);
}

#[kithara::test(tokio)]
#[case(WaitingReason::Waiting)]
#[case(WaitingReason::WaitingDemand)]
#[case(WaitingReason::WaitingMetadata)]
async fn decoder_node_downstream_park_after_audio_waits_for_the_preload_quota(
    #[case] reason: WaitingReason,
) {
    let source = ScriptedSource::new([produced(), TrackStep::Blocked(reason)]);
    let (mut node, _receiver, _lane) = prepared_node(source, 1, 2).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(!node.source.is_preloaded(), "one chunk is below quota");
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(
        !node.source.is_preloaded(),
        "a park cannot replace the quota"
    );
}

#[kithara::test(tokio)]
#[case(WaitingReason::Waiting)]
#[case(WaitingReason::WaitingDemand)]
#[case(WaitingReason::WaitingMetadata)]
async fn decoder_node_park_without_audio_keeps_the_preload_gate_shut(
    #[case] reason: WaitingReason,
) {
    let (mut node, _receiver, _lane) =
        prepared_node(ScriptedSource::new([TrackStep::Blocked(reason)]), 2, 1).await;
    let _ = node.tick();
    assert!(
        !node.source.is_preloaded(),
        "a park with nothing emitted is not preload"
    );
}

#[kithara::test(tokio)]
async fn decoder_node_preload_gate_stays_shut_below_the_chunk_quota() {
    let (mut node, _receiver, _lane) = prepared_node(ScriptedSource::new([produced()]), 4, 2).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(
        !node.source.is_preloaded(),
        "one chunk of a two-chunk quota is not preload"
    );
}

#[kithara::test(tokio)]
async fn decoder_node_seek_rearms_preload_gate() {
    let source = ScriptedSource::new([
        produced(),
        TrackStep::StateChanged,
        produced_at(SEGMENT_FROM),
    ]);
    let (mut node, mut receiver, mut lane) = prepared_node(source, 1, 1).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.source.is_preloaded(), "first chunk opens preload");
    let id = segment(&mut lane);
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(!node.source.is_preloaded(), "new segment rearms preload");
    assert_eq!(node.source.cursor().segment, id);
    assert!(node.pending.is_none(), "StateChanged is not admitted PCM");
    assert!(
        matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if packet.meta.segment == SegmentId::FIRST)
    );
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(
        node.source.is_preloaded(),
        "new-segment refill opens preload"
    );
    assert!(matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if packet.meta.segment == id));
    assert!(lane.receipts().any(|receipt| matches!(receipt.outcome(), kithara_command::Outcome::Applied { data, .. } if data.ready == Some(id))));
}
