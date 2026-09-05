use std::sync::atomic::{AtomicBool, Ordering};

use kithara_audio::{AudioSource, Fetch, PreloadGate, ProducerPort, TrackStep, WaitingReason};
use kithara_events::{DeferredBus, Event, EventBus};
use kithara_platform::{
    CancelToken,
    sync::Arc,
    thread::{park_timeout, sleep as thread_sleep},
    time::{Duration, Instant, timeout as platform_timeout},
};
use kithara_signal::{AudioChunk, AudioChunkInfo};
use kithara_stream::{PlayheadState, PlayheadWrite, SeekControl, SeekObserve, SeekState};
use kithara_test_utils::kithara;
use kithara_worker::{Dispatcher, DispatcherConfig, TaskConfig, TaskHandle, Worker, WorkerConfig};

use super::*;
use crate::{
    test_pools::{Pools, pools, sample_buffer},
    worker::scheduler::ServiceClass,
};

fn empty_chunk(pools: &Pools) -> AudioChunk {
    AudioChunk::new(AudioChunkInfo::default(), sample_buffer(pools, &[]))
}

struct MockSource {
    pools: Pools,
    seek: Arc<dyn SeekControl>,
    seek_obs: Arc<dyn SeekObserve>,
    ready: bool,
    should_panic: bool,
    chunks_to_produce: usize,
    cursor: usize,
}

impl MockSource {
    fn new(pools: Pools, chunks: usize) -> Self {
        let state = Arc::new(SeekState::new());
        let seek = Arc::clone(&state) as Arc<dyn SeekControl>;
        let seek_obs = Arc::clone(&state) as Arc<dyn SeekObserve>;
        Self {
            pools,
            seek,
            seek_obs,
            chunks_to_produce: chunks,
            cursor: 0,
            ready: true,
            should_panic: false,
        }
    }

    fn not_ready(pools: Pools, chunks: usize) -> Self {
        Self {
            ready: false,
            ..Self::new(pools, chunks)
        }
    }

    fn panicking(pools: Pools) -> Self {
        Self {
            should_panic: true,
            ..Self::new(pools, 100)
        }
    }
}

impl AudioSource for MockSource {
    type Chunk = AudioChunk;

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek_obs)
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if self.seek_obs.is_pending() || self.seek_obs.is_flushing() {
            let epoch = self.seek_obs.epoch();
            self.seek.complete(epoch);
            self.seek.clear_pending(epoch);
            return TrackStep::StateChanged;
        }
        if !self.ready {
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        if self.should_panic {
            panic!("mock panic for testing");
        }
        if self.cursor >= self.chunks_to_produce {
            return TrackStep::Eof;
        }
        self.cursor += 1;
        TrackStep::Produced(Fetch::data(empty_chunk(&self.pools), 0))
    }
}

struct FailingSource {
    seek_obs: Arc<dyn SeekObserve>,
}

impl Default for FailingSource {
    fn default() -> Self {
        Self {
            seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        }
    }
}

impl AudioSource for FailingSource {
    type Chunk = AudioChunk;

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek_obs)
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        TrackStep::Failed
    }
}

fn make_node<S>(
    source: S,
    ringbuf_capacity: usize,
    preload_chunks: usize,
) -> (
    DecoderNode<S>,
    impl FnMut() -> Option<Fetch<AudioChunk>> + Send + 'static,
    Arc<PreloadGate>,
)
where
    S: AudioSource<Chunk = AudioChunk>,
{
    let (port, pop) = ProducerPort::probe(ringbuf_capacity);
    let preload_gate = Arc::new(PreloadGate::default());
    let seek_obs = source.seek_observe();
    let seek_epoch = seek_obs.epoch();
    let node = DecoderNode {
        seek_obs,
        source,
        port,
        preload_chunks,
        emit: Arc::new(DeferredBus::<Event>::new(EventBus::new(8), 8)),
        playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadWrite>,
        preload_gate: Arc::clone(&preload_gate),
        runtime: DecoderRuntime {
            seek_epoch,
            ..Default::default()
        },
        engine_load: None,
    };
    (node, pop, preload_gate)
}

fn wait_for_chunks(
    pop: &mut impl FnMut() -> Option<Fetch<AudioChunk>>,
    count: usize,
    timeout: Duration,
) -> usize {
    let start = Instant::now();
    let mut received = 0;
    while received < count && start.elapsed() < timeout {
        if pop().is_some() {
            received += 1;
        } else {
            thread_sleep(Duration::from_millis(1));
        }
    }
    received
}

struct PlaybackScheduler {
    dispatcher: Dispatcher,
    _worker: Worker,
}

impl PlaybackScheduler {
    fn register<S>(&self, node: DecoderNode<S>) -> Result<TaskHandle, kithara_worker::TaskError>
    where
        S: AudioSource<Chunk = AudioChunk>,
    {
        self.dispatcher.register(
            TaskConfig::new().with_priority(ServiceClass::Audible.into()),
            |_| node,
        )
    }

    fn start(name: String, cancel: CancelToken, capacity: std::num::NonZeroUsize) -> Self {
        let worker = Worker::new(WorkerConfig::new().with_cancel(cancel));
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name(name)
                .capacity(capacity)
                .observer(crate::worker::scheduler::PlaybackObserver::default())
                .build(),
        );
        Self {
            dispatcher,
            _worker: worker,
        }
    }

    fn unregister(&self, task: TaskHandle) {
        drop(task);
    }

    fn wake_handle(&self) -> kithara_worker::Wake {
        self.dispatcher.wake_handle()
    }
}

fn test_scheduler() -> PlaybackScheduler {
    PlaybackScheduler::start(
        "kithara-play-worker-test".into(),
        CancelToken::never(),
        std::num::NonZeroUsize::new(8).expect("test capacity is non-zero"),
    )
}

fn scheduler_with_capacity(capacity: usize) -> PlaybackScheduler {
    PlaybackScheduler::start(
        "kithara-play-worker-capacity-test".into(),
        CancelToken::never(),
        std::num::NonZeroUsize::new(capacity).expect("test capacity is non-zero"),
    )
}

fn register<S>(handle: &PlaybackScheduler, node: DecoderNode<S>) -> TaskHandle
where
    S: AudioSource<Chunk = AudioChunk>,
{
    handle
        .register(node)
        .expect("test playback task must register")
}

#[kithara::test]
fn worker_delivers_chunks() {
    let pools = pools();
    let handle = test_scheduler();
    let (node, mut pop, _) = make_node(MockSource::new(pools.clone(), 10), 32, 3);
    let _id = register(&handle, node);

    let received = wait_for_chunks(&mut pop, 5, Duration::from_secs(5));
    assert!(received >= 5, "expected at least 5 chunks, got {received}");
}

#[kithara::test]
fn worker_multi_track_round_robin() {
    let pools = pools();
    let handle = test_scheduler();
    let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1);
    let (node_b, mut pop_b, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1);
    let _id_a = register(&handle, node_a);
    let _id_b = register(&handle, node_b);

    let a = wait_for_chunks(&mut pop_a, 3, Duration::from_secs(5));
    let b = wait_for_chunks(&mut pop_b, 3, Duration::from_secs(5));
    assert!(a >= 3, "track A expected at least 3 chunks, got {a}");
    assert!(b >= 3, "track B expected at least 3 chunks, got {b}");
}

#[kithara::test]
fn worker_skips_not_ready_tracks() {
    let pools = pools();
    let handle = test_scheduler();
    let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1);
    let (node_b, mut pop_b, _) = make_node(MockSource::not_ready(pools.clone(), 10), 32, 1);
    let _id_a = register(&handle, node_a);
    let _id_b = register(&handle, node_b);

    thread_sleep(Duration::from_millis(100));

    let a = wait_for_chunks(&mut pop_a, 1, Duration::from_millis(100));
    let b = wait_for_chunks(&mut pop_b, 1, Duration::from_millis(50));
    assert!(a >= 1, "ready track should receive chunks");
    assert_eq!(b, 0, "not-ready track should receive nothing");
}

#[kithara::test]
fn worker_overflow_on_full_ringbuf() {
    let pools = pools();
    let handle = test_scheduler();
    let (node, mut pop, _) = make_node(MockSource::new(pools.clone(), 5), 1, 1);
    let _id = register(&handle, node);

    thread_sleep(Duration::from_millis(50));
    assert!(pop().is_some(), "should have at least one chunk");
    thread_sleep(Duration::from_millis(50));
    assert!(pop().is_some(), "overflow slot should have been flushed");
}

#[kithara::test]
fn worker_panic_isolation() {
    let pools = pools();
    let handle = test_scheduler();
    let (node_a, _, _) = make_node(MockSource::panicking(pools.clone()), 32, 1);
    let (node_b, mut pop_b, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1);
    let _id_a = register(&handle, node_a);
    let _id_b = register(&handle, node_b);

    let b = wait_for_chunks(&mut pop_b, 3, Duration::from_secs(5));
    assert!(b >= 3, "sibling should survive a node panic, got {b}");
}

#[kithara::test]
fn worker_seek_enters_pending_reset() {
    let pools = pools();
    let handle = test_scheduler();
    let source = MockSource::new(pools.clone(), 100);
    let seek = Arc::clone(&source.seek);
    let (node, mut pop, _) = make_node(source, 32, 1);
    let _id = register(&handle, node);

    assert!(wait_for_chunks(&mut pop, 2, Duration::from_secs(5)) >= 2);
    let _ = seek.begin(Duration::from_secs(10));
    handle.wake_handle().wake();
    thread_sleep(Duration::from_millis(100));
    assert!(
        wait_for_chunks(&mut pop, 1, Duration::from_secs(5)) >= 1,
        "decoding should resume after seek"
    );
}

#[kithara::test(tokio)]
#[case::progress(10, 3, "preload gate must open at the threshold")]
#[case::eof(0, 8, "early EOF must open the preload gate")]
async fn worker_preload_gate_fires(
    #[case] chunks: usize,
    #[case] preload: usize,
    #[case] message: &str,
) {
    let pools = pools();
    let handle = test_scheduler();
    let (node, _pop, gate) = make_node(MockSource::new(pools.clone(), chunks), 32, preload);
    let _id = register(&handle, node);

    platform_timeout(Duration::from_secs(1), gate.wait())
        .await
        .expect(message);
    assert!(gate.is_ready());
}

#[kithara::test(tokio)]
async fn worker_preload_gate_fires_on_failure() {
    let handle = test_scheduler();
    let (node, _pop, gate) = make_node(FailingSource::default(), 32, 8);
    let _id = register(&handle, node);

    platform_timeout(Duration::from_secs(1), gate.wait())
        .await
        .expect("decoder failure must open the preload gate");
    assert!(gate.is_ready());
}

#[kithara::test(tokio)]
async fn worker_preload_gate_reopens_after_seek() {
    let pools = pools();
    let handle = test_scheduler();
    let source = MockSource::new(pools.clone(), 10);
    let seek = Arc::clone(&source.seek);
    let (node, _pop, gate) = make_node(source, 32, 1);
    let _id = register(&handle, node);

    platform_timeout(Duration::from_secs(1), gate.wait())
        .await
        .expect("initial preload gate must open");

    let epoch = seek.begin(Duration::from_secs(1));
    handle.wake_handle().wake();
    platform_timeout(Duration::from_secs(1), gate.wait_for_epoch(epoch))
        .await
        .expect("post-seek gate must reopen");
}

#[kithara::test]
fn worker_unregister_removes_track() {
    let pools = pools();
    let handle = test_scheduler();
    let (node, mut pop, _) = make_node(MockSource::new(pools.clone(), 100), 32, 1);
    let id = register(&handle, node);

    assert!(wait_for_chunks(&mut pop, 2, Duration::from_secs(5)) >= 2);
    handle.unregister(id);
    thread_sleep(Duration::from_millis(50));
    while pop().is_some() {}
    thread_sleep(Duration::from_millis(50));
    assert!(pop().is_none(), "no chunks should arrive after unregister");
}

#[kithara::test]
fn unregister_one_task_keeps_sibling_running_and_releases_capacity() {
    let pools = pools();
    let handle = scheduler_with_capacity(2);
    let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 100), 1, 1);
    let (node_b, mut pop_b, _) = make_node(MockSource::new(pools.clone(), 100), 1, 1);

    let id_a = register(&handle, node_a);
    let id_b = register(&handle, node_b);
    assert_eq!(wait_for_chunks(&mut pop_a, 1, Duration::from_secs(1)), 1);
    assert_eq!(wait_for_chunks(&mut pop_b, 1, Duration::from_secs(1)), 1);

    handle.unregister(id_a);
    let (node_c, _, _) = make_node(MockSource::new(pools.clone(), 1), 1, 1);
    let id_c = handle
        .register(node_c)
        .expect("unregister must release capacity");

    while pop_b().is_some() {}
    handle.wake_handle().wake();
    assert_eq!(
        wait_for_chunks(&mut pop_b, 1, Duration::from_secs(1)),
        1,
        "unregistering one task must not stop its sibling"
    );

    handle.unregister(id_b);
    handle.unregister(id_c);
}

#[kithara::test]
fn shared_worker_blocking_track_does_not_starve_producing_track() {
    let pools = pools();
    struct BlockingSource {
        seek_obs: Arc<dyn SeekObserve>,
        blocking: Arc<AtomicBool>,
    }

    impl AudioSource for BlockingSource {
        type Chunk = AudioChunk;

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            if self.blocking.load(Ordering::Relaxed) {
                thread_sleep(Duration::from_millis(10));
            }
            TrackStep::Blocked(WaitingReason::Waiting)
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_obs)
        }
    }

    let handle = test_scheduler();
    let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 100), 32, 0);
    let _id_a = register(&handle, node_a);

    let blocking = Arc::new(AtomicBool::new(true));
    let blocking_source = BlockingSource {
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        blocking: Arc::clone(&blocking),
    };
    let (node_b, _pop_b, _) = make_node(blocking_source, 32, 0);
    let _id_b = register(&handle, node_b);

    thread_sleep(Duration::from_millis(500));
    let mut got_a = 0;
    while pop_a().is_some() {
        got_a += 1;
    }
    assert!(
        got_a >= 11,
        "producing track was starved: got {got_a} chunks"
    );

    blocking.store(false, Ordering::Relaxed);
}

#[kithara::test]
fn shared_worker_sync_blocking_step_starves_other_tracks() {
    let pools = pools();
    const SOURCE_CHUNKS: u32 = 1000;
    const POLL_BUDGET: u32 = 600;
    const BLOCK_MS: u64 = 10;

    struct SlowDecodeSource {
        pools: Pools,
        seek_obs: Arc<dyn SeekObserve>,
        block_ms: u64,
    }

    impl AudioSource for SlowDecodeSource {
        type Chunk = AudioChunk;

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            thread_sleep(Duration::from_millis(self.block_ms));
            TrackStep::Produced(Fetch::data(empty_chunk(&self.pools), 0))
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_obs)
        }
    }

    let handle = test_scheduler();
    let (node_a, mut pop_a, _) = make_node(
        MockSource::new(pools.clone(), SOURCE_CHUNKS as usize),
        32,
        0,
    );
    let _id_a = register(&handle, node_a);

    let slow_source = SlowDecodeSource {
        pools: pools.clone(),
        seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        block_ms: BLOCK_MS,
    };
    let (node_b, mut pop_b, _) = make_node(slow_source, 32, 0);
    let _id_b = register(&handle, node_b);

    let mut delivered = 0u32;
    let mut polls = 0u32;
    let mut deepest_poll = 0u32;

    while delivered < SOURCE_CHUNKS && polls < POLL_BUDGET {
        let mut this_poll = 0u32;
        while pop_a().is_some() {
            delivered += 1;
            this_poll += 1;
        }
        deepest_poll = deepest_poll.max(this_poll);
        while pop_b().is_some() {}
        polls += 1;
        park_timeout(Duration::from_millis(5));
    }

    assert!(
        delivered >= SOURCE_CHUNKS,
        "fast track drained {delivered} of {SOURCE_CHUNKS} chunks in {polls} polls; deepest poll {deepest_poll}, peer blocked {BLOCK_MS}ms"
    );
}
