use std::num::NonZeroU32;

use kithara_audio::{
    AudioSource, Fetch, PreloadGate, ProducerPort, SourceEnd, TrackStep, WaitingReason,
    mock::AudioSourceMock,
};
use kithara_events::{AudioEvent, DeferredBus, Event, EventBus};
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{
    PlayheadRead, PlayheadState, PlayheadWrite, SeekControl, SeekObserve, SeekState,
};
use kithara_test_utils::kithara;
use kithara_worker::{Task, TickResult};
use unimock::{MockFn, Unimock, matching};

use super::*;
use crate::{
    effects::EffectDrain,
    test_pools::{Pools, pools, sample_buffer},
    worker::{EngineLoad, WarpSource},
};

fn empty_chunk(pools: &Pools) -> AudioChunk {
    AudioChunk::new(AudioChunkInfo::default(), sample_buffer(pools, &[]))
}

fn test_node<S>(
    source: S,
    port: ProducerPort,
    preload_gate: Arc<PreloadGate>,
    seek_obs: Arc<dyn SeekObserve>,
) -> DecoderNode<S> {
    DecoderNode {
        seek_obs,
        source,
        port,
        preload_gate,
        playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadWrite>,
        emit: Arc::new(DeferredBus::new(EventBus::new(8), 8)),
        preload_chunks: 1,
        engine_load: None,
        runtime: DecoderRuntime::default(),
    }
}

struct PersistentEofSource {
    seek: Arc<SeekState>,
}

struct CommitSource {
    chunk: Option<AudioChunk>,
    commits: Arc<Mutex<Vec<(SourceEnd, u64)>>>,
    seek: Arc<SeekState>,
    source_end: SourceEnd,
}

impl AudioSource for CommitSource {
    type Chunk = AudioChunk;

    fn commit_source_end(&mut self, source_end: SourceEnd, epoch: u64) {
        self.commits.lock().push((source_end, epoch));
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.chunk.take().map_or(TrackStep::Eof, |chunk| {
            TrackStep::Produced(Fetch::rendered(chunk, 7, self.source_end))
        })
    }
}

impl AudioSource for PersistentEofSource {
    type Chunk = AudioChunk;

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        TrackStep::Eof
    }
}

#[kithara::test]
fn decoder_node_eof_under_backpressure() {
    let pools = pools();
    let gate = Arc::new(PreloadGate::default());
    let (mut port, mut pop) = ProducerPort::probe(1);

    port.push_direct(Fetch::data(empty_chunk(&pools), 0));

    let source = Unimock::new((
        AudioSourceMock::step_track.stub(|each| {
            each.call(matching!()).answers(&|_| TrackStep::Eof);
        }),
        AudioSourceMock::decode_epoch.stub(|each| {
            each.call(matching!()).returns(0u64);
        }),
    ));

    let bus = EventBus::new(8);
    let mut events = bus.subscribe();
    let mut node = test_node(
        source,
        port,
        gate,
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    );
    node.emit = Arc::new(DeferredBus::new(bus, 8));

    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(!node.runtime.eof_sent);

    assert!(pop().is_some(), "the queued data must drain first");

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.runtime.eof_sent);
    assert!(matches!(pop(), Some(Fetch::NaturalEof { .. })));
    assert_eq!(node.tick(), TickResult::Backpressured);

    node.emit.flush();
    let end_events = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|envelope| matches!(envelope.event, Event::Audio(AudioEvent::EndOfStream { .. })))
        .count();
    assert_eq!(end_events, 1, "current-epoch EOF must publish exactly once");
}

#[kithara::test]
fn decoder_node_does_not_republish_exhausted_warp_source_eof() {
    let pools = pools();
    let seek = Arc::new(SeekState::new());
    let source = PersistentEofSource {
        seek: Arc::clone(&seek),
    };
    let effects = Vec::new();
    let drain = EffectDrain::new(effects.len(), &pools)
        .unwrap_or_else(|error| panic!("test effect drain: {error}"));
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
    let config = kithara_warp::WarpConfig::builder().build();
    let warp = kithara_warp::Warp::new((), &config);
    let renderer = warp.renderer(spec, pools.clone());
    let source = WarpSource::new(source, renderer, effects, drain, spec, pools);
    let (port, mut pop) = ProducerPort::probe(1);
    let bus = EventBus::new(8);
    let mut events = bus.subscribe();
    let mut node = test_node(
        source,
        port,
        Arc::new(PreloadGate::default()),
        seek as Arc<dyn SeekObserve>,
    );
    node.emit = Arc::new(DeferredBus::new(bus, 8));

    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(matches!(pop(), Some(Fetch::NaturalEof { .. })));
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(node.tick(), TickResult::Backpressured);

    node.emit.flush();
    let end_events = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|envelope| matches!(envelope.event, Event::Audio(AudioEvent::EndOfStream { .. })))
        .count();
    assert_eq!(end_events, 1);
}

#[kithara::test]
fn decoder_node_records_engine_load_on_produced() {
    let pools = pools();
    use std::num::NonZero;

    use kithara_signal::AudioSpec;

    let meter = Arc::new(EngineLoad::default());
    assert!(!meter.snapshot().is_active(), "idle before any tick");

    let (port, _pop) = ProducerPort::probe(4);
    let chunk = AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec {
                channels: 2,
                sample_rate: NonZero::new(44_100).unwrap(),
            },
            frames: 4_410,
            ..Default::default()
        },
        sample_buffer(&pools, &vec![0.0f32; 4_410 * 2]),
    );
    let source = Unimock::new(
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(chunk, 0))),
    );

    let mut node = DecoderNode {
        source,
        port,
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        preload_gate: Arc::new(PreloadGate::default()),
        playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadWrite>,
        emit: Arc::new(DeferredBus::new(EventBus::new(8), 8)),
        preload_chunks: 1,
        engine_load: Some(Arc::clone(&meter)),
        runtime: DecoderRuntime::default(),
    };

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(
        meter.snapshot().is_active(),
        "engine meter records on a Produced tick: {:?}",
        meter.snapshot()
    );
}

#[kithara::test]
fn worker_telemetry_throttles_immediate_repeats() {
    let (port, _pop) = ProducerPort::probe(4);
    let source = Unimock::new(());
    let gate = Arc::new(PreloadGate::default());
    let seek = Arc::new(SeekState::new());
    let playhead = Arc::new(PlayheadState::new());
    playhead.set_position(Duration::from_millis(100));
    playhead.set_decoded_frontier(Duration::from_millis(350));
    let bus = EventBus::new(8);
    let mut events = bus.subscribe();
    let emit = Arc::new(DeferredBus::new(bus, 8));
    let meter = Arc::new(EngineLoad::default());
    meter.record(Duration::from_millis(5), 4_410, 44_100);

    let mut node = DecoderNode {
        source,
        port,
        seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
        preload_gate: gate,
        playhead: Arc::clone(&playhead) as Arc<dyn PlayheadWrite>,
        emit: Arc::clone(&emit),
        preload_chunks: 1,
        engine_load: Some(meter),
        runtime: DecoderRuntime::default(),
    };

    let now = Instant::now();
    node.maybe_emit_worker_telemetry(now);
    node.maybe_emit_worker_telemetry(now);
    emit.flush();

    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(Event::Audio(AudioEvent::BufferHealth {
            buffered_ms: 250,
            decoded_frontier_ms: 350,
            seek_epoch: 0,
        }))
    ));
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(Event::Audio(AudioEvent::EngineLoad { .. }))
    ));
    assert!(
        events.try_recv().is_err(),
        "second immediate tick stays throttled"
    );
}

#[kithara::test]
fn decoder_node_distinguishes_failed_from_eof_on_the_wire() {
    fn drain_marker(pop: &mut impl FnMut() -> Option<Fetch<AudioChunk>>) -> Fetch<AudioChunk> {
        pop().expect("producer pushed a terminal marker")
    }

    let gate = Arc::new(PreloadGate::default());

    let (eof_port, mut eof_pop) = ProducerPort::probe(1);
    let eof_source = Unimock::new((
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Eof),
        AudioSourceMock::decode_epoch.stub(|each| {
            each.call(matching!()).returns(0u64);
        }),
    ));
    let mut eof_node = test_node(
        eof_source,
        eof_port,
        Arc::clone(&gate),
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    );
    assert_eq!(eof_node.tick(), TickResult::Progress);
    let eof_marker = drain_marker(&mut eof_pop);

    let (failed_port, mut failed_pop) = ProducerPort::probe(1);
    let failed_source = Unimock::new((
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Failed),
        AudioSourceMock::decode_epoch.stub(|each| {
            each.call(matching!()).returns(0u64);
        }),
    ));
    let mut failed_node = test_node(
        failed_source,
        failed_port,
        gate,
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    );
    let _ = failed_node.tick();
    let failed_marker = drain_marker(&mut failed_pop);

    assert!(matches!(eof_marker, Fetch::NaturalEof { .. }));
    assert!(matches!(failed_marker, Fetch::Failure { .. }));
}

#[kithara::test]
fn eof_marker_and_deferred_event_keep_the_decode_epoch() {
    let gate = Arc::new(PreloadGate::default());
    let (port, mut pop) = ProducerPort::probe(1);

    let seek_state = Arc::new(SeekState::new());
    let seek_obs = Arc::clone(&seek_state) as Arc<dyn SeekObserve>;

    let source = Unimock::new((
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Eof),
        AudioSourceMock::decode_epoch
            .next_call(matching!())
            .returns(0u64),
    ));

    let bus = EventBus::new(8);
    let mut events = bus.subscribe();
    let mut node = test_node(source, port, gate, seek_obs);
    node.emit = Arc::new(DeferredBus::new(bus, 8));
    assert_eq!(node.tick(), TickResult::Progress);

    let live_epoch = seek_state.begin(Duration::from_secs(1));
    assert_eq!(live_epoch, 1, "seek overtakes the deferred EOF flush");

    let marker = pop().expect("producer pushed an EOF marker");
    assert!(matches!(&marker, Fetch::NaturalEof { .. }));
    assert_eq!(
        marker.epoch(),
        0,
        "EOF marker must carry the producer decode epoch"
    );
    node.emit.flush();
    let mut eof_epochs =
        std::iter::from_fn(|| events.try_recv().ok()).filter_map(|envelope| match envelope.event {
            Event::Audio(AudioEvent::EndOfStream { seek_epoch }) => Some(seek_epoch),
            _ => None,
        });
    assert_eq!(eof_epochs.next(), Some(0));
    assert_eq!(eof_epochs.next(), None);
}

#[kithara::test]
fn decoded_frontier_advances_only_after_final_port_admission() {
    let pools = pools();
    let (mut port, mut pop) = ProducerPort::probe(1);
    port.push_direct(Fetch::data(empty_chunk(&pools), 0));
    let end = Duration::from_millis(750);
    let mut chunk = empty_chunk(&pools);
    chunk.meta.end_timestamp = end;
    let source = Unimock::new(
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(chunk, 0))),
    );
    let playhead = Arc::new(PlayheadState::new());
    let mut node = test_node(
        source,
        port,
        Arc::new(PreloadGate::default()),
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    );
    node.playhead = Arc::clone(&playhead) as Arc<dyn PlayheadWrite>;

    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(playhead.decoded_frontier(), Duration::ZERO);

    assert!(pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(playhead.decoded_frontier(), end);
}

#[kithara::test]
fn source_end_commits_only_after_final_port_admission() {
    let pools = pools();
    let (mut port, mut pop) = ProducerPort::probe(1);
    port.push_direct(Fetch::data(empty_chunk(&pools), 0));
    let source_end = SourceEnd::new(
        12_345,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let commits = Arc::new(Mutex::new(Vec::new()));
    let source = CommitSource {
        chunk: Some(empty_chunk(&pools)),
        commits: Arc::clone(&commits),
        seek: Arc::new(SeekState::new()),
        source_end,
    };
    let mut node = test_node(
        source,
        port,
        Arc::new(PreloadGate::default()),
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    );

    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(commits.lock().is_empty());

    assert!(pop().is_some());
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(commits.lock().as_slice(), &[(source_end, 7)]);
}

#[kithara::test]
fn decoder_node_preload_gate_waits_for_ring_capacity() {
    let pools = pools();
    let gate = Arc::new(PreloadGate::default());
    let (mut port, mut pop) = ProducerPort::probe(1);

    port.push_direct(Fetch::data(empty_chunk(&pools), 0));

    let source = Unimock::new(
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(empty_chunk(&pools), 0))),
    );

    let mut node = test_node(
        source,
        port,
        Arc::clone(&gate),
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    );

    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(node.runtime.chunks_sent, 0);
    assert!(!node.runtime.preloaded);
    assert!(!gate.is_ready());

    assert!(pop().is_some());

    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.runtime.chunks_sent, 1);
    assert!(node.runtime.preloaded);
    assert!(gate.is_ready());
}

#[kithara::test]
fn decoder_node_live_upstream_demand_does_not_tick_hang_wait() {
    let gate = Arc::new(PreloadGate::default());
    let (port, _pop) = ProducerPort::probe(2);

    let source = Unimock::new(
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Blocked(WaitingReason::WaitingDemand)),
    );

    let mut node = test_node(
        source,
        port,
        gate,
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
    );

    assert_eq!(node.tick(), TickResult::UpstreamPending);
}

#[kithara::test]
fn decoder_node_seek_rearms_preload_gate() {
    let pools = pools();
    let gate = Arc::new(PreloadGate::default());
    let (port, mut pop) = ProducerPort::probe(1);

    let seek_state = Arc::new(SeekState::new());
    let source = Unimock::new((
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(empty_chunk(&pools), 0))),
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::StateChanged),
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(empty_chunk(&pools), 0))),
    ));

    let mut node = test_node(
        source,
        port,
        Arc::clone(&gate),
        Arc::clone(&seek_state) as Arc<dyn SeekObserve>,
    );

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.runtime.preloaded);
    assert!(gate.is_ready(), "first chunk opens the gate");

    let epoch = SeekControl::begin(&*seek_state, Duration::from_secs(1));

    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(!node.runtime.preloaded, "seek resets the preload runtime");
    assert!(!gate.is_ready(), "sync_seek_epoch closes the gate");

    assert!(
        pop().is_some(),
        "consumer discards the stale pre-seek chunk"
    );

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(
        !node.runtime.preloaded,
        "source first applies the seek epoch"
    );

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.runtime.preloaded);
    assert!(gate.is_ready(), "post-seek refill reopens the gate");
    assert!(
        gate.is_ready_for_epoch(epoch),
        "post-seek refill must open the new seek epoch"
    );
}
