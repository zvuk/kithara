pub(super) use std::num::{NonZeroU32, NonZeroUsize};

pub(super) use kithara_audio::{
    AudioSource, Fetch, SeekOutcome, SourceEnd, TrackStep, WaitingReason,
};
pub(super) use kithara_command::{Batch, ChannelConfig, Sender, When, channel};
pub(super) use kithara_effects::EffectDrain;
pub(super) use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
pub(super) use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, SegmentId};
pub(super) use kithara_test_fixtures::unit_fixtures::eq_silence as node_silence;
pub(super) use kithara_test_utils::kithara;
pub(super) use kithara_worker::{Task, TickResult};

use super::*;
pub(super) use crate::{
    LaneCommand, LaneProtocol, WarpSource,
    test_pools::{Pools, TestPools, pools},
    worker::{
        EngineLoad, PcmReceiver,
        packet_tests::{PacketRing, chunk},
    },
};

pub(in crate::worker) async fn prepared_node<T>(
    source: T,
    capacity: usize,
    preload: usize,
) -> (DecoderNode<T, TestPools>, PcmReceiver, Sender<LaneProtocol>)
where
    T: AudioSource<Chunk = AudioChunk>,
{
    let pools = pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let (lane, inbox) = channel(ChannelConfig::builder().build());
    let config = kithara_warp::WarpConfig::builder()
        .source_block_frames(NonZeroUsize::new(65_536).expect("source block"))
        .build();
    let source = WarpSource::new(
        source,
        kithara_warp::Warp::new((), &config).renderer(spec, pools.clone()),
        Vec::new(),
        EffectDrain::new(0, &pools).expect("drain"),
        spec,
        pools.clone(),
        crate::LaneSetup {
            inbox,
            preload_chunks: NonZeroUsize::new(preload).expect("preload"),
            declick: crate::consts::DEFAULT_DECLICK,
        },
    );
    let (receiver, producer) = PacketRing::new(spec, Duration::from_secs(1), capacity).into_ends();
    (
        DecoderNode::new(
            source,
            producer,
            None,
            AudioChunkInfo {
                spec,
                ..AudioChunkInfo::default()
            },
            None,
            pools,
            None,
        ),
        receiver,
        lane,
    )
}

pub(in crate::worker) fn empty_chunk(_pools: &Pools) -> AudioChunk {
    chunk(
        AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate")),
        SegmentId::FIRST,
        0,
        0,
        &[0.0, 0.0],
    )
}

pub(in crate::worker) struct ScriptedSource {
    pub(in crate::worker) steps: std::collections::VecDeque<TrackStep<AudioChunk>>,
    commits: Arc<Mutex<Vec<SourceEnd>>>,
}

impl ScriptedSource {
    pub(in crate::worker) fn new(steps: impl IntoIterator<Item = TrackStep<AudioChunk>>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            commits: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl AudioSource for ScriptedSource {
    type Chunk = AudioChunk;
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, kithara_audio::AudioReadError> {
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }
    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }
    fn commit_source_end(&mut self, end: SourceEnd, _meta: AudioChunkInfo) {
        self.commits.lock().push(end);
    }
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.steps.pop_front().unwrap_or(TrackStep::Eof)
    }
}

pub(super) fn produced() -> TrackStep<AudioChunk> {
    TrackStep::Produced(Fetch::data(empty_chunk(&pools())))
}
pub(in crate::worker) fn segment(lane: &mut Sender<LaneProtocol>) -> SegmentId {
    let id = SegmentId::FIRST.next();
    lane.send(
        When::Next,
        Batch {
            basis: Vec::new(),
            commands: vec![LaneCommand::Segment {
                id,
                from: Duration::from_secs(1),
                speed: kithara_warp::SpeedCurve::Constant(1.0),
            }],
        },
    )
    .expect("segment batch");
    id
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
        assert_eq!(receiver.cached_span(), Duration::from_millis(250));
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
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(node.terminal.is_none());
    assert!(matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if !packet.meta.end_of_track));
    assert_eq!(node.tick(), TickResult::Progress);
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
        assert_eq!(receiver.cached_span(), Duration::from_millis(250));
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
    assert_eq!(receiver.decoded_frontier(), initial);
    assert!(receiver.pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(receiver.decoded_frontier(), Duration::from_millis(750));
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
    assert!(commits.lock().is_empty());
    assert!(receiver.pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(commits.lock().as_slice(), &[end]);
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
    let source = ScriptedSource::new([produced(), TrackStep::StateChanged, produced()]);
    let (mut node, mut receiver, mut lane) = prepared_node(source, 1, 1).await;
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.source.is_preloaded(), "first chunk opens preload");
    let id = segment(&mut lane);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(!node.source.is_preloaded(), "new segment rearms preload");
    assert_eq!(node.source.cursor().segment, id);
    assert!(
        matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if packet.meta.segment == SegmentId::FIRST)
    );
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(
        !node.source.is_preloaded(),
        "StateChanged is not admitted PCM"
    );
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(
        node.source.is_preloaded(),
        "new-segment refill opens preload"
    );
    assert!(matches!(receiver.pop(), Some(PcmPacket::Chunk(packet)) if packet.meta.segment == id));
    assert!(lane.receipts().any(|receipt| matches!(receipt.outcome(), kithara_command::Outcome::Applied { data, .. } if data.ready == Some(id))));
}
