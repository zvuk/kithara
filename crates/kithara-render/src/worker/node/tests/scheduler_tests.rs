use std::num::NonZeroU32;

use kithara_audio::{AudioSource, Fetch, SeekOutcome, TrackStep, WaitingReason};
use kithara_command::Sender;
use kithara_platform::{
    CancelToken, thread,
    time::{Duration, timeout as platform_timeout},
};
use kithara_signal::AudioChunk;
use kithara_test_utils::kithara;
use kithara_worker::{Dispatcher, DispatcherConfig, TaskConfig, TaskHandle, Worker, WorkerConfig};

use super::*;
use crate::{
    LaneProtocol, ServiceClass,
    test_pools::{Pools, TestPools, pools},
    worker::PcmPacket,
};

fn empty_chunk(pools: &Pools, frame: u64) -> AudioChunk {
    let mut chunk = tests::empty_chunk(pools);
    chunk.meta.frame_offset = frame;
    chunk.meta.source_span =
        kithara_signal::SourceSpan::new(frame, frame + 1, chunk.spec().sample_rate, 1);
    chunk.meta.timestamp = chunk.spec().duration_for(frame).expect("timestamp");
    chunk.meta.end_timestamp = chunk.spec().duration_for(frame + 1).expect("end timestamp");
    chunk
}

struct MockSource {
    pools: Pools,
    ready: bool,
    should_panic: bool,
    chunks_to_produce: usize,
    cursor: usize,
}

impl MockSource {
    fn new(pools: Pools, chunks: usize) -> Self {
        Self {
            pools,
            chunks_to_produce: chunks,
            cursor: 0,
            ready: true,
            should_panic: false,
        }
    }
}

impl AudioSource for MockSource {
    type Chunk = AudioChunk;
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, kithara_audio::AudioReadError> {
        self.cursor = 0;
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }
    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if !self.ready {
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        if self.should_panic {
            panic!("mock panic for testing");
        }
        if self.cursor >= self.chunks_to_produce {
            return TrackStep::Eof;
        }
        let frame = u64::try_from(self.cursor).expect("source cursor fits");
        self.cursor += 1;
        TrackStep::Produced(Fetch::data(empty_chunk(&self.pools, frame)))
    }
}

#[derive(Default)]
struct FailingSource;

impl AudioSource for FailingSource {
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

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        TrackStep::Failed(TrackFailureKind::Decode {
            kind: kithara_audio::DecodeErrorKind::InvalidData,
        })
    }
}

async fn make_node<S>(
    source: S,
    ringbuf_capacity: usize,
    preload_chunks: usize,
) -> (
    DecoderNode<S, TestPools>,
    impl FnMut() -> Option<()> + Send + 'static,
    Sender<LaneProtocol>,
)
where
    S: AudioSource<Chunk = AudioChunk>,
{
    let (node, mut receiver, lane) =
        prepared_node(source, ringbuf_capacity, preload_chunks.max(1)).await;
    let pop = move || match receiver.pop() {
        Some(PcmPacket::Chunk(packet)) if !packet.meta.end_of_track => Some(()),
        _ => None,
    };
    (node, pop, lane)
}

struct PlaybackScheduler {
    dispatcher: Dispatcher,
    _worker: Worker,
}

impl PlaybackScheduler {
    fn register<S>(
        &self,
        node: DecoderNode<S, TestPools>,
    ) -> Result<TaskHandle, kithara_worker::TaskError>
    where
        S: AudioSource<Chunk = AudioChunk>,
    {
        self.dispatcher.register(
            TaskConfig::new().with_priority(ServiceClass::Audible.into()),
            |_| node,
        )
    }

    fn start(name: String, cancel: CancelToken, capacity: NonZeroUsize) -> Self {
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
        NonZeroUsize::new(8).expect("test capacity is non-zero"),
    )
}

fn register<S>(handle: &PlaybackScheduler, node: DecoderNode<S, TestPools>) -> TaskHandle
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
    let (mut node, _receiver, _lane) =
        prepared_node(MockSource::new(pools(), chunks), 32, preload).await;
    platform_timeout(Duration::from_secs(1), preload::preload(&mut node))
        .await
        .expect(message)
        .expect("preload succeeds");
    assert!(node.source.is_preloaded() || node.terminal.is_some());
}

#[kithara::test(tokio)]
async fn worker_preload_reports_failure() {
    let (mut node, mut receiver, _lane) = prepared_node(FailingSource, 32, 8).await;
    assert!(
        platform_timeout(Duration::from_secs(1), preload(&mut node))
            .await
            .expect("failure terminates preload")
            .is_err()
    );
    assert!(matches!(receiver.pop(), Some(PcmPacket::Failed { .. })));
}

#[kithara::test(tokio)]
async fn worker_preload_gate_reopens_after_seek() {
    let (mut node, _receiver, mut lane) = prepared_node(MockSource::new(pools(), 10), 32, 1).await;
    platform_timeout(Duration::from_secs(1), preload(&mut node))
        .await
        .expect("initial preload")
        .expect("preload succeeds");
    assert!(node.source.is_preloaded());
    let id = segment(&mut lane);
    node.synchronize().expect("new segment");
    assert!(!node.source.is_preloaded());
    platform_timeout(Duration::from_secs(1), preload(&mut node))
        .await
        .expect("post-segment preload")
        .expect("preload succeeds");
    assert!(node.source.is_preloaded());
    assert!(lane.receipts().any(|receipt| matches!(receipt.outcome(), kithara_command::Outcome::Applied { data, .. } if data.ready == Some(id))));
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
        /// Time each step holds the shared worker thread before producing.
        step: Duration,
        pools: Pools,
        cursor: u64,
    }

    impl AudioSource for EndlessSource {
        type Chunk = AudioChunk;
        fn seek(&mut self, target: Duration) -> Result<SeekOutcome, kithara_audio::AudioReadError> {
            self.cursor = 0;
            Ok(SeekOutcome::Landed {
                target,
                landed_at: target,
            })
        }
        fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}
        fn host_sample_rate(&self) -> Option<NonZeroU32> {
            None
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            thread::sleep(self.step);
            let chunk = empty_chunk(&self.pools, self.cursor);
            self.cursor += 1;
            TrackStep::Produced(Fetch::data(chunk))
        }
    }

    /// Source that never has data, so its node waits on every pass.
    struct WaitingSource {
        /// Time each step holds the shared worker thread before waiting.
        step: Duration,
    }

    impl AudioSource for WaitingSource {
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

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            thread::sleep(self.step);
            TrackStep::Blocked(WaitingReason::Waiting)
        }
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
        drop(trace);
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
        let (node, mut pop, mut lane) = make_node(source, 32, 1).await;
        let _id = register(&handle, node);

        receive_chunks(&trace, &handle, &mut pop, 2).await;
        let seen = trace.events().len();
        pass_after(&trace, &handle, seen, |pass| {
            pass_field(pass, "backpressured") >= 1
        })
        .await;
        let seen = trace.events().len();
        let id = segment(&mut lane);
        handle.wake_handle().wake();
        // The consumer must observe the seek and retire the full pre-seek ring.
        let _ = pop();
        trace
            .wait_for(|events| {
                events[seen..]
                    .iter()
                    .any(|e| is(e, "chunk_admitted") && e.field("segment") == Some(id.get()))
            })
            .await;
        receive_chunks(&trace, &handle, &mut pop, 1).await;
        drop(trace);
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
        drop(trace);
        assert!(pop().is_none(), "no chunks should arrive after unregister");
    }

    #[kithara::test(tokio, flash(false))]
    async fn unregister_one_task_keeps_sibling_running_and_releases_capacity() {
        let trace = scope();
        let pools = pools();
        let handle = PlaybackScheduler::start(
            "kithara-play-worker-capacity-test".into(),
            CancelToken::never(),
            NonZeroUsize::new(2).expect("test capacity is non-zero"),
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
        let (node_b, _pop_b, _) = make_node(WaitingSource { step }, 32, 0).await;
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
                cursor: 0,
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
                break;
            }
            admitted_after(&trace, &handle, seen).await;
        }
        drop(trace);
    }
}
