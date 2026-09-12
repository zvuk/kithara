use std::num::NonZeroU32;

use kithara_assets::AssetStore;
use kithara_audio::{
    Audio, AudioConfig, AudioEvent, AudioRead, AudioSource, ChunkOutcome, Fetch,
    NoResamplerBackend, PreloadGate, SourceEnd, TrackStep, WaitingReason, mock::AudioSourceMock,
};
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{
    PlayheadRead, PlayheadState, PlayheadWrite, SeekControl, SeekObserve, SeekState, Stream,
    WorkerWake,
};
use kithara_test_fixtures::{assets, unit_fixtures::eq_silence as node_silence};
use kithara_test_utils::kithara;
use kithara_worker::{Task, TickResult};
use unimock::{MockFn, Unimock, matching};

use super::*;
use crate::{
    effects::EffectDrain,
    test_pools::{Pools, pools, sample_buffer},
    worker::{EngineLoad, WarpSource},
};

struct TestWorkerWake;

impl WorkerWake for TestWorkerWake {
    fn defer(&self) {}

    fn wake(&self) {}
}

pub(super) async fn prepared_node<S>(
    source: S,
    capacity: usize,
    preload_chunks: usize,
) -> (
    DecoderNode<S>,
    Audio<Stream<kithara_file::File<crate::test_pools::TestPools>>>,
)
where
    S: AudioSource<Chunk = AudioChunk>,
{
    let pools = pools();
    let path = assets::signal_wav_sine440_120ms()
        .path()
        .expect("native WAV fixture path");
    let stream = kithara_file::FileConfig::for_src(kithara_file::FileSrc::Local(path.to_owned()))
        .store(AssetStore::builder(pools.clone()).build())
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<_, NoResamplerBackend>::for_stream(stream)
        .audio_buffer_chunks(capacity)
        .preload_chunks(
            std::num::NonZeroUsize::new(preload_chunks).expect("non-zero preload threshold"),
        )
        .build();
    let prepared = Audio::prepare(config, Arc::new(TestWorkerWake), pools)
        .await
        .unwrap_or_else(|error| panic!("prepare real audio lane: {error}"))
        .map(|audio, _| (audio, source));
    let (audio, lane) = prepared.into();
    let node = DecoderNode {
        source: lane.source,
        port: lane.port,
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        preload_gate: lane.preload_gate,
        playhead: lane.playhead,
        emit: lane.emit,
        preload_chunks: lane.preload_chunks,
        engine_load: None,
        runtime: DecoderRuntime::default(),
    };
    (node, audio)
}

fn empty_chunk(pools: &Pools) -> AudioChunk {
    AudioChunk::new(AudioChunkInfo::default(), sample_buffer(pools, &[]))
}

struct PersistentEofSource {
    seek: Arc<SeekState>,
}

struct OneChunkSource {
    seek: Arc<SeekState>,
    chunk: Option<AudioChunk>,
}

struct CommitSource {
    commits: Arc<Mutex<Vec<(SourceEnd, u64)>>>,
    seek: Arc<SeekState>,
    leading_chunk: Option<AudioChunk>,
    chunk: Option<AudioChunk>,
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
        if let Some(chunk) = self.leading_chunk.take() {
            return TrackStep::Produced(Fetch::data(chunk, 0));
        }
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

impl AudioSource for OneChunkSource {
    type Chunk = AudioChunk;

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.chunk.take().map_or(TrackStep::Eof, |chunk| {
            TrackStep::Produced(Fetch::data(chunk, 0))
        })
    }
}

#[kithara::test(tokio)]
async fn decoder_node_eof_under_backpressure() {
    let pools = pools();
    let source = OneChunkSource {
        seek: Arc::new(SeekState::new()),
        chunk: Some(empty_chunk(&pools)),
    };

    let bus = EventBus::new(8);
    let mut events = bus.subscribe();
    let (mut node, mut audio) = prepared_node(source, 1, 1).await;
    node.emit = Arc::new(DeferredBus::new(bus, 8));

    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(!node.runtime.eof_sent);

    assert!(matches!(audio.next_chunk(), Ok(ChunkOutcome::Chunk(_))));

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.runtime.eof_sent);
    assert!(matches!(audio.next_chunk(), Ok(ChunkOutcome::Eof { .. })));
    assert_eq!(node.tick(), TickResult::Backpressured);

    node.emit.flush();
    let end_events = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|envelope| matches!(envelope.event, AudioEvent::EndOfStream { .. }))
        .count();
    assert_eq!(end_events, 1, "current-epoch EOF must publish exactly once");
}

#[kithara::test(tokio)]
async fn decoder_node_does_not_republish_exhausted_warp_source_eof() {
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
    let bus = EventBus::new(8);
    let mut events = bus.subscribe();
    let (mut node, mut audio) = prepared_node(source, 1, 1).await;
    node.emit = Arc::new(DeferredBus::new(bus, 8));

    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Progress);
    assert!(matches!(audio.next_chunk(), Ok(ChunkOutcome::Eof { .. })));
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(node.tick(), TickResult::Backpressured);

    node.emit.flush();
    let end_events = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|envelope| matches!(envelope.event, AudioEvent::EndOfStream { .. }))
        .count();
    assert_eq!(end_events, 1);
}

#[kithara::test(tokio)]
async fn decoder_node_records_engine_load_on_produced(node_silence: Vec<f32>) {
    let pools = pools();
    use std::num::NonZero;

    use kithara_signal::AudioSpec;

    let meter = Arc::new(EngineLoad::default());
    assert!(!meter.snapshot().is_active(), "idle before any tick");

    let chunk = AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec {
                channels: 2,
                sample_rate: NonZero::new(44_100).unwrap(),
            },
            frames: 4_410,
            ..Default::default()
        },
        sample_buffer(&pools, &node_silence),
    );
    let source = Unimock::new(
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(chunk, 0))),
    );

    let (mut node, _audio) = prepared_node(source, 4, 1).await;
    node.engine_load = Some(Arc::clone(&meter));

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(
        meter.snapshot().is_active(),
        "engine meter records on a Produced tick: {:?}",
        meter.snapshot()
    );
}

#[kithara::test(tokio)]
async fn worker_telemetry_throttles_immediate_repeats() {
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

    let (mut node, _audio) = prepared_node(source, 4, 1).await;
    node.seek_obs = Arc::clone(&seek) as Arc<dyn SeekObserve>;
    node.preload_gate = gate;
    node.playhead = Arc::clone(&playhead) as Arc<dyn PlayheadWrite>;
    node.emit = Arc::clone(&emit);
    node.engine_load = Some(meter);

    let now = Instant::now();
    node.maybe_emit_worker_telemetry(now);
    node.maybe_emit_worker_telemetry(now);
    emit.flush();

    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(AudioEvent::BufferHealth {
            buffered_ms: 250,
            decoded_frontier_ms: 350,
            seek_epoch: 0,
        })
    ));
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(AudioEvent::EngineLoad { .. })
    ));
    assert!(
        events.try_recv().is_err(),
        "second immediate tick stays throttled"
    );
}

#[kithara::test(tokio)]
async fn decoder_node_distinguishes_failed_from_eof_on_the_wire() {
    let eof_source = Unimock::new((
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Eof),
        AudioSourceMock::decode_epoch.stub(|each| {
            each.call(matching!()).returns(0u64);
        }),
    ));
    let (mut eof_node, mut eof_audio) = prepared_node(eof_source, 1, 1).await;
    assert_eq!(eof_node.tick(), TickResult::Progress);
    let eof_marker = eof_audio.next_chunk();

    let failed_source = Unimock::new((
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Failed),
        AudioSourceMock::decode_epoch.stub(|each| {
            each.call(matching!()).returns(0u64);
        }),
    ));
    let (mut failed_node, mut failed_audio) = prepared_node(failed_source, 1, 1).await;
    let _ = failed_node.tick();
    let failed_marker = failed_audio.next_chunk();

    assert!(matches!(eof_marker, Ok(ChunkOutcome::Eof { .. })));
    assert!(failed_marker.is_err());
}

#[kithara::test(tokio)]
async fn deferred_eof_event_keeps_the_decode_epoch() {
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
    let (mut node, _audio) = prepared_node(source, 1, 1).await;
    node.seek_obs = seek_obs;
    node.emit = Arc::new(DeferredBus::new(bus, 8));
    assert_eq!(node.tick(), TickResult::Progress);

    let live_epoch = seek_state.begin(Duration::from_secs(1));
    assert_eq!(live_epoch, 1, "seek overtakes the deferred EOF flush");

    node.emit.flush();
    let mut eof_epochs =
        std::iter::from_fn(|| events.try_recv().ok()).filter_map(|envelope| match envelope.event {
            AudioEvent::EndOfStream { seek_epoch } => Some(seek_epoch),
            _ => None,
        });
    assert_eq!(eof_epochs.next(), Some(0));
    assert_eq!(eof_epochs.next(), None);
}

#[kithara::test(tokio)]
async fn decoded_frontier_advances_only_after_final_port_admission() {
    let pools = pools();
    let end = Duration::from_millis(750);
    let mut chunk = empty_chunk(&pools);
    chunk.meta.end_timestamp = end;
    let source = Unimock::new((
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(empty_chunk(&pools), 0))),
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Produced(Fetch::data(chunk, 0))),
    ));
    let playhead = Arc::new(PlayheadState::new());
    let (mut node, mut audio) = prepared_node(source, 1, 1).await;
    node.playhead = Arc::clone(&playhead) as Arc<dyn PlayheadWrite>;

    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert_eq!(playhead.decoded_frontier(), Duration::ZERO);

    assert!(matches!(audio.next_chunk(), Ok(ChunkOutcome::Chunk(_))));
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(playhead.decoded_frontier(), end);
}

#[kithara::test(tokio)]
async fn source_end_commits_only_after_final_port_admission() {
    let pools = pools();
    let source_end = SourceEnd::new(
        12_345,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let commits = Arc::new(Mutex::new(Vec::new()));
    let source = CommitSource {
        source_end,
        leading_chunk: Some(empty_chunk(&pools)),
        chunk: Some(empty_chunk(&pools)),
        commits: Arc::clone(&commits),
        seek: Arc::new(SeekState::new()),
    };
    let (mut node, mut audio) = prepared_node(source, 1, 1).await;

    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(commits.lock().is_empty());

    assert!(matches!(audio.next_chunk(), Ok(ChunkOutcome::Chunk(_))));
    assert_eq!(node.tick(), TickResult::Progress);
    assert_eq!(commits.lock().as_slice(), &[(source_end, 7)]);
}

#[kithara::test(tokio)]
async fn decoder_node_live_upstream_demand_does_not_tick_hang_wait() {
    let source = Unimock::new(
        AudioSourceMock::step_track
            .next_call(matching!())
            .returns(TrackStep::Blocked(WaitingReason::WaitingDemand)),
    );

    let (mut node, _audio) = prepared_node(source, 2, 1).await;

    assert_eq!(node.tick(), TickResult::UpstreamPending);
}

#[kithara::test(tokio)]
async fn decoder_node_seek_rearms_preload_gate() {
    let pools = pools();
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

    let (mut node, mut audio) = prepared_node(source, 1, 1).await;
    let gate = Arc::clone(&node.preload_gate);
    node.seek_obs = Arc::clone(&seek_state) as Arc<dyn SeekObserve>;

    assert_eq!(node.tick(), TickResult::Progress);
    assert!(node.runtime.preloaded);
    assert!(gate.is_ready(), "first chunk opens the gate");

    let epoch = SeekControl::begin(&*seek_state, Duration::from_secs(1));

    assert_eq!(node.tick(), TickResult::Backpressured);
    assert!(!node.runtime.preloaded, "seek resets the preload runtime");
    assert!(!gate.is_ready(), "sync_seek_epoch closes the gate");

    assert!(
        matches!(audio.next_chunk(), Ok(ChunkOutcome::Chunk(_))),
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
