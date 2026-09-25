use kithara_audio::{
    AudioRead, AudioSource, ChunkOutcome, Fetch, PreloadGate, TrackStep, WaitingReason,
};
use kithara_platform::{
    CancelToken,
    sync::Arc,
    thread,
    time::{Duration, timeout as platform_timeout},
};
use kithara_signal::{AudioChunk, AudioChunkInfo};
use kithara_stream::{SeekControl, SeekObserve, SeekState};
use kithara_test_utils::kithara;
use kithara_worker::{Dispatcher, DispatcherConfig, TaskConfig, TaskHandle, Worker, WorkerConfig};

use super::{tests::prepared_node, *};
use crate::{
    test_pools::{Pools, pools, sample_buffer},
    worker::scheduler::ServiceClass,
};

fn empty_chunk(pools: &Pools) -> AudioChunk {
    AudioChunk::new(AudioChunkInfo::default(), sample_buffer(pools, &[]))
}

struct MockSource {
    seek: Arc<dyn SeekControl>,
    seek_obs: Arc<dyn SeekObserve>,
    pools: Pools,
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

async fn make_node<S>(
    source: S,
    ringbuf_capacity: usize,
    preload_chunks: usize,
) -> (
    DecoderNode<S>,
    impl FnMut() -> Option<()> + Send + 'static,
    Arc<PreloadGate>,
)
where
    S: AudioSource<Chunk = AudioChunk>,
{
    let seek_obs = source.seek_observe();
    let seek_epoch = seek_obs.epoch();
    let (mut node, mut audio) =
        prepared_node(source, ringbuf_capacity, preload_chunks.max(1)).await;
    node.seek_obs = seek_obs;
    node.runtime.seek_epoch = seek_epoch;
    let preload_gate = Arc::clone(&node.preload_gate);
    let pop = move || match audio.next_chunk() {
        Ok(ChunkOutcome::Chunk(_)) => Some(()),
        Ok(ChunkOutcome::Pending { .. } | ChunkOutcome::Eof { .. }) | Err(_) => None,
    };
    (node, pop, preload_gate)
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

fn register<S>(handle: &PlaybackScheduler, node: DecoderNode<S>) -> TaskHandle
where
    S: AudioSource<Chunk = AudioChunk>,
{
    handle
        .register(node)
        .expect("test playback task must register")
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
    let (node, _pop, gate) = make_node(MockSource::new(pools.clone(), chunks), 32, preload).await;
    let _id = register(&handle, node);

    platform_timeout(Duration::from_secs(1), gate.wait())
        .await
        .expect(message);
    assert!(gate.is_ready());
}

#[kithara::test(tokio)]
async fn worker_preload_gate_fires_on_failure() {
    let handle = test_scheduler();
    let (node, _pop, gate) = make_node(FailingSource::default(), 32, 8).await;
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
    let (node, _pop, gate) = make_node(source, 32, 1).await;
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

/// Scheduler contracts observed through the product's `chunk_admitted` and
/// `scheduler_pass` probes.
#[cfg(feature = "usdt")]
mod probed {
    use kithara_test_utils::test::usdt::{ProbeEvent, Scope, scope};

    use super::*;

    impl MockSource {
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

    /// Source that always produces, so its node competes for every pass.
    struct EndlessSource {
        seek_obs: Arc<dyn SeekObserve>,
        /// Time each step holds the shared worker thread before producing.
        step: Duration,
        pools: Pools,
    }

    impl AudioSource for EndlessSource {
        type Chunk = AudioChunk;

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_obs)
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            thread::sleep(self.step);
            TrackStep::Produced(Fetch::data(empty_chunk(&self.pools), 0))
        }
    }

    /// Source that never has data, so its node waits on every pass.
    struct WaitingSource {
        seek_obs: Arc<dyn SeekObserve>,
        /// Time each step holds the shared worker thread before waiting.
        step: Duration,
    }

    impl AudioSource for WaitingSource {
        type Chunk = AudioChunk;

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_obs)
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            thread::sleep(self.step);
            TrackStep::Blocked(WaitingReason::Waiting)
        }
    }

    fn new_seek() -> Arc<dyn SeekObserve> {
        Arc::new(SeekState::new()) as Arc<dyn SeekObserve>
    }

    fn is(event: &ProbeEvent, probe: &str) -> bool {
        event.probe == probe
    }

    fn pass_field(event: &ProbeEvent, field: &str) -> u64 {
        event
            .field(field)
            .expect("scheduler_pass carries its counts")
    }

    /// Waits for a chunk admission recorded after `seen` probes.
    async fn admitted_after(trace: &Scope, handle: &PlaybackScheduler, seen: usize) {
        handle.wake_handle().wake();
        trace
            .wait_for(|events| events[seen..].iter().any(|e| is(e, "chunk_admitted")))
            .await;
    }

    /// Waits for a scheduler pass recorded after `seen` probes that satisfies `holds`.
    async fn pass_after<F>(trace: &Scope, handle: &PlaybackScheduler, seen: usize, holds: F)
    where
        F: Fn(&ProbeEvent) -> bool,
    {
        handle.wake_handle().wake();
        trace
            .wait_for(|events| {
                events[seen..]
                    .iter()
                    .any(|e| is(e, "scheduler_pass") && holds(e))
            })
            .await;
    }

    async fn receive_chunks<P>(trace: &Scope, handle: &PlaybackScheduler, pop: &mut P, count: usize)
    where
        P: FnMut() -> Option<()>,
    {
        let mut received = 0;
        loop {
            let seen = trace.events().len();
            while received < count && pop().is_some() {
                received += 1;
            }
            if received == count {
                return;
            }
            admitted_after(trace, handle, seen).await;
        }
    }

    #[kithara::test(tokio, flash(false))]
    async fn worker_delivers_chunks() {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node, mut pop, _) = make_node(MockSource::new(pools.clone(), 10), 32, 3).await;
        let _id = register(&handle, node);

        receive_chunks(&trace, &handle, &mut pop, 5).await;
    }

    #[kithara::test(tokio, flash(false))]
    async fn worker_multi_track_round_robin() {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1).await;
        let (node_b, mut pop_b, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1).await;
        let _id_a = register(&handle, node_a);
        let _id_b = register(&handle, node_b);

        receive_chunks(&trace, &handle, &mut pop_a, 3).await;
        receive_chunks(&trace, &handle, &mut pop_b, 3).await;
    }

    #[kithara::test(tokio, flash(false))]
    async fn worker_skips_not_ready_tracks() {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1).await;
        let (node_b, mut pop_b, _) =
            make_node(MockSource::not_ready(pools.clone(), 10), 32, 1).await;
        let _id_a = register(&handle, node_a);
        let _id_b = register(&handle, node_b);

        receive_chunks(&trace, &handle, &mut pop_a, 1).await;
        pass_after(&trace, &handle, 0, |pass| pass_field(pass, "waiting") >= 1).await;
        assert!(pop_b().is_none(), "not-ready track should receive nothing");
    }

    #[kithara::test(tokio, flash(false))]
    async fn worker_overflow_on_full_ringbuf() {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node, mut pop, _) = make_node(MockSource::new(pools.clone(), 5), 1, 1).await;
        let _id = register(&handle, node);

        receive_chunks(&trace, &handle, &mut pop, 1).await;
        let seen = trace.events().len();
        pass_after(&trace, &handle, seen, |pass| {
            pass_field(pass, "backpressured") >= 1
        })
        .await;
        receive_chunks(&trace, &handle, &mut pop, 1).await;
    }

    #[kithara::test(tokio, flash(false))]
    async fn worker_panic_isolation() {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node_a, _, _) = make_node(MockSource::panicking(pools.clone()), 32, 1).await;
        let (node_b, mut pop_b, _) = make_node(MockSource::new(pools.clone(), 10), 32, 1).await;
        let _id_a = register(&handle, node_a);
        let _id_b = register(&handle, node_b);

        receive_chunks(&trace, &handle, &mut pop_b, 3).await;
    }

    #[kithara::test(tokio, flash(false))]
    async fn worker_seek_enters_pending_reset() {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let source = MockSource::new(pools.clone(), 100);
        let seek = Arc::clone(&source.seek);
        let (node, mut pop, _) = make_node(source, 32, 1).await;
        let _id = register(&handle, node);

        receive_chunks(&trace, &handle, &mut pop, 2).await;
        let seen = trace.events().len();
        pass_after(&trace, &handle, seen, |pass| {
            pass_field(pass, "backpressured") >= 1
        })
        .await;
        let seen = trace.events().len();
        let epoch = seek.begin(Duration::from_secs(10));
        handle.wake_handle().wake();
        // The consumer must observe the seek and retire the full pre-seek ring.
        let _ = pop();
        trace
            .wait_for(|events| {
                events[seen..]
                    .iter()
                    .any(|e| is(e, "chunk_admitted") && e.field("epoch") == Some(epoch))
            })
            .await;
        receive_chunks(&trace, &handle, &mut pop, 1).await;
    }

    #[kithara::test(tokio, flash(false))]
    async fn worker_unregister_removes_track() {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node, mut pop, _) = make_node(MockSource::new(pools.clone(), 100), 32, 1).await;
        let id = register(&handle, node);

        receive_chunks(&trace, &handle, &mut pop, 2).await;
        let seen = trace.events().len();
        drop(id);
        pass_after(&trace, &handle, seen, |pass| {
            pass_field(pass, "active") == 0
        })
        .await;
        while pop().is_some() {}
        let seen = trace.events().len();
        pass_after(&trace, &handle, seen, |_| true).await;
        assert!(pop().is_none(), "no chunks should arrive after unregister");
    }

    #[kithara::test(tokio, flash(false))]
    async fn unregister_one_task_keeps_sibling_running_and_releases_capacity() {
        let trace = scope();
        let pools = pools();
        let handle = PlaybackScheduler::start(
            "kithara-play-worker-capacity-test".into(),
            CancelToken::never(),
            std::num::NonZeroUsize::new(2).expect("test capacity is non-zero"),
        );
        let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 100), 1, 1).await;
        let (node_b, mut pop_b, _) = make_node(MockSource::new(pools.clone(), 100), 1, 1).await;

        let id_a = register(&handle, node_a);
        let id_b = register(&handle, node_b);
        receive_chunks(&trace, &handle, &mut pop_a, 1).await;
        receive_chunks(&trace, &handle, &mut pop_b, 1).await;

        drop(id_a);
        let (node_c, _, _) = make_node(MockSource::new(pools.clone(), 1), 1, 1).await;
        let id_c = handle
            .register(node_c)
            .expect("unregister must release capacity");

        while pop_b().is_some() {}
        receive_chunks(&trace, &handle, &mut pop_b, 1).await;

        drop(id_b);
        drop(id_c);
    }

    #[kithara::test(tokio, flash(false))]
    #[case::instant_step(Duration::ZERO)]
    #[case::sync_blocking_step(Duration::from_millis(10))]
    async fn shared_worker_waiting_track_does_not_starve_producing_track(#[case] step: Duration) {
        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node_a, mut pop_a, _) = make_node(MockSource::new(pools.clone(), 100), 32, 0).await;
        let _id_a = register(&handle, node_a);
        let (node_b, _pop_b, _) = make_node(
            WaitingSource {
                step,
                seek_obs: new_seek(),
            },
            32,
            0,
        )
        .await;
        let _id_b = register(&handle, node_b);

        pass_after(&trace, &handle, 0, |pass| pass_field(pass, "waiting") >= 1).await;
        receive_chunks(&trace, &handle, &mut pop_a, 64).await;
    }

    #[kithara::test(tokio, flash(false))]
    #[case::instant_step(Duration::ZERO)]
    #[case::sync_blocking_step(Duration::from_millis(10))]
    async fn shared_worker_endless_producer_does_not_starve_other_tracks(#[case] step: Duration) {
        const SOURCE_CHUNKS: usize = 1000;

        let trace = scope();
        let pools = pools();
        let handle = test_scheduler();
        let (node_a, mut pop_a, _) =
            make_node(MockSource::new(pools.clone(), SOURCE_CHUNKS), 32, 0).await;
        let _id_a = register(&handle, node_a);
        let (node_b, mut pop_b, _) = make_node(
            EndlessSource {
                step,
                pools: pools.clone(),
                seek_obs: new_seek(),
            },
            32,
            0,
        )
        .await;
        let _id_b = register(&handle, node_b);

        let mut delivered = 0;
        loop {
            let seen = trace.events().len();
            while pop_a().is_some() {
                delivered += 1;
            }
            while pop_b().is_some() {}
            if delivered == SOURCE_CHUNKS {
                return;
            }
            admitted_after(&trace, &handle, seen).await;
        }
    }
}
