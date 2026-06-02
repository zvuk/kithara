use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_decode::PcmChunk;
use kithara_platform::CancellationToken;

use super::{
    AudioWorkerSource, PreloadGate,
    decoder_node::DecoderNode,
    hang_observer::HangWatchdogObserver,
    types::{TrackId, TrackIdGen},
};
use crate::{
    pipeline::fetch::Fetch,
    runtime::{AtomicServiceClass, Scheduler, SchedulerHandle},
};

/// Everything needed to register a track with the shared worker.
pub(crate) struct TrackRegistration {
    pub(crate) preload_gate: Arc<PreloadGate>,
    /// Shared priority hint. The real-time consumer writes it wait-free
    /// (`Audio::set_service_class`); the worker scheduler reads it each pass.
    pub(crate) service_class: Arc<AtomicServiceClass>,
    pub(crate) source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
    /// Spent-chunk return ring: the real-time consumer ([`crate::Audio`])
    /// hands every consumed `PcmChunk` here instead of dropping it, so the
    /// pooled buffer is freed/recycled on the worker thread rather than on
    /// the audio thread. See `crates/kithara-audio/README.md`.
    pub(crate) trash_inlet: crate::runtime::Inlet<PcmChunk>,
    pub(crate) outlet: crate::runtime::Outlet<Fetch<PcmChunk>>,
    pub(crate) preload_chunks: usize,
}

/// Clonable handle to a shared audio worker.
///
/// Multiple [`Audio`](crate::Audio) handles can share one worker by cloning
/// the handle and passing it via [`AudioConfig`](crate::AudioConfig).
pub struct AudioWorkerHandle {
    id_gen: Arc<TrackIdGen>,
    inner: SchedulerHandle<Box<dyn crate::runtime::Node>>,
}

impl Clone for AudioWorkerHandle {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            id_gen: Arc::clone(&self.id_gen),
        }
    }
}

/// Monotonic counter for unique audio-worker thread names.
static AUDIO_WORKER_ID: AtomicU64 = AtomicU64::new(0);

impl AudioWorkerHandle {
    /// Register a track. Returns the assigned [`TrackId`].
    ///
    /// If the worker thread has already exited (e.g. after shutdown), the
    /// registration is silently lost and the returned track will produce no
    /// data. Callers must ensure the worker is alive before registering.
    pub(crate) fn register_track(&self, reg: TrackRegistration) -> TrackId {
        let id = self.id_gen.next();
        let node: Box<dyn crate::runtime::Node> = Box::new(DecoderNode::from(reg));
        self.inner.register(id, node);
        id
    }

    /// Request graceful shutdown and cancel the worker.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Remove a track by ID.
    pub(crate) fn unregister_track(&self, track_id: TrackId) {
        self.inner.unregister(track_id);
    }

    /// Wake the worker (e.g. when new data arrives from downloader).
    pub fn wake(&self) {
        self.inner.wake();
    }

    /// Spawn a new shared worker thread bound to the given [`CancellationToken`] and
    /// return a handle. Production callers (e.g. `EngineImpl`) pass a
    /// `child_token()` of the player master so worker shutdown participates in
    /// the unified cancel hierarchy and the produce-core's lock-free
    /// `is_cancelled()` read observes a master cancel.
    #[must_use]
    pub fn with_cancel(cancel: CancellationToken) -> Self {
        let id = AUDIO_WORKER_ID.fetch_add(1, Ordering::Relaxed);
        let inner = Scheduler::<Box<dyn crate::runtime::Node>, HangWatchdogObserver>::start(
            format!("kithara-audio-worker-{id}"),
            HangWatchdogObserver::new(),
            cancel,
        );

        Self {
            inner,
            id_gen: Arc::new(TrackIdGen::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use kithara_decode::PcmChunk;
    use kithara_platform::{
        thread::sleep as thread_sleep,
        time::{Instant, timeout as platform_timeout},
    };
    use kithara_stream::{SeekControl, SeekObserve, Timeline};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        pipeline::track_fsm::{TrackStep, WaitingReason},
        runtime::connect,
        worker::{AudioWorkerSource, thread_wake::ThreadWake, types::ServiceClass},
    };

    struct MockSource {
        seek: Arc<dyn SeekControl>,
        seek_obs: Arc<dyn SeekObserve>,
        ready: bool,
        should_panic: bool,
        chunks_to_produce: usize,
        cursor: usize,
    }

    impl MockSource {
        fn new(chunks: usize) -> Self {
            let tl = Timeline::new();
            let seek = tl.seek_control();
            let seek_obs = tl.seek_observe();
            Self {
                seek,
                seek_obs,
                chunks_to_produce: chunks,
                cursor: 0,
                ready: true,
                should_panic: false,
            }
        }

        fn not_ready(chunks: usize) -> Self {
            Self {
                ready: false,
                ..Self::new(chunks)
            }
        }

        fn panicking() -> Self {
            Self {
                should_panic: true,
                ..Self::new(100)
            }
        }
    }

    impl AudioWorkerSource for MockSource {
        type Chunk = PcmChunk;

        fn step_track(&mut self) -> TrackStep<PcmChunk> {
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
            TrackStep::Produced(Fetch::new(PcmChunk::default(), false, 0))
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_obs)
        }
    }

    struct FailingSource {
        seek_obs: Arc<dyn SeekObserve>,
    }

    impl Default for FailingSource {
        fn default() -> Self {
            Self {
                seek_obs: Timeline::new().seek_observe(),
            }
        }
    }

    impl AudioWorkerSource for FailingSource {
        type Chunk = PcmChunk;

        fn step_track(&mut self) -> TrackStep<PcmChunk> {
            TrackStep::Failed
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_obs)
        }
    }

    fn make_registration<S>(
        source: S,
        ringbuf_capacity: usize,
        preload_chunks: usize,
    ) -> (
        TrackRegistration,
        crate::runtime::Inlet<Fetch<PcmChunk>>,
        Arc<PreloadGate>,
    )
    where
        S: AudioWorkerSource<Chunk = PcmChunk> + 'static,
    {
        let wake = Arc::new(ThreadWake::default());
        let (outlet, inlet) = connect::<Fetch<PcmChunk>>(ringbuf_capacity, Some(wake.clone()));
        let (_trash_outlet, trash_inlet) = connect::<PcmChunk>(ringbuf_capacity + 2, None);
        let preload_gate = Arc::new(PreloadGate::default());

        let reg = TrackRegistration {
            outlet,
            trash_inlet,
            preload_chunks,
            source: Box::new(source),
            preload_gate: Arc::clone(&preload_gate),
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::Audible)),
        };
        (reg, inlet, preload_gate)
    }

    fn wait_for_chunks(
        rx: &mut crate::runtime::Inlet<Fetch<PcmChunk>>,
        count: usize,
        timeout: Duration,
    ) -> usize {
        let start = Instant::now();
        let mut received = 0;
        while received < count && start.elapsed() < timeout {
            if rx.try_pop().is_some() {
                received += 1;
            } else {
                thread_sleep(Duration::from_millis(1));
            }
        }
        received
    }

    #[kithara::test]
    fn worker_creates_and_drops_cleanly() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());
        thread_sleep(Duration::from_millis(10));
        handle.shutdown();
        thread_sleep(Duration::from_millis(50));
    }

    #[kithara::test]
    fn worker_delivers_chunks() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());
        let (reg, mut data_rx, _preload_gate) = make_registration(MockSource::new(10), 32, 3);

        let _id = handle.register_track(reg);

        let received = wait_for_chunks(&mut data_rx, 5, Duration::from_secs(5));
        assert!(received >= 5, "expected >=5 chunks, got {received}");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_multi_track_round_robin() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(10), 32, 1);
        let (reg_b, mut rx_b, _) = make_registration(MockSource::new(10), 32, 1);

        let _id_a = handle.register_track(reg_a);
        let _id_b = handle.register_track(reg_b);

        let a = wait_for_chunks(&mut rx_a, 3, Duration::from_secs(5));
        let b = wait_for_chunks(&mut rx_b, 3, Duration::from_secs(5));
        assert!(a >= 3, "track A: expected >=3 chunks, got {a}");
        assert!(b >= 3, "track B: expected >=3 chunks, got {b}");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_skips_not_ready_tracks() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(10), 32, 1);
        let (reg_b, mut rx_b, _) = make_registration(MockSource::not_ready(10), 32, 1);

        let _id_a = handle.register_track(reg_a);
        let _id_b = handle.register_track(reg_b);

        thread_sleep(Duration::from_millis(100));

        let a = wait_for_chunks(&mut rx_a, 1, Duration::from_millis(100));
        let b = wait_for_chunks(&mut rx_b, 1, Duration::from_millis(50));
        assert!(a >= 1, "track A should receive chunks");
        assert_eq!(b, 0, "track B should receive nothing (not ready)");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_overflow_on_full_ringbuf() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg, mut rx, _) = make_registration(MockSource::new(5), 1, 1);

        let _id = handle.register_track(reg);

        thread_sleep(Duration::from_millis(50));

        let first = rx.try_pop();
        assert!(first.is_some(), "should have at least one chunk");

        thread_sleep(Duration::from_millis(50));

        let second = rx.try_pop();
        assert!(second.is_some(), "overflow slot should have been flushed");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_panic_isolation() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg_a, _, _) = make_registration(MockSource::panicking(), 32, 1);
        let (reg_b, mut rx_b, _) = make_registration(MockSource::new(10), 32, 1);

        let _id_a = handle.register_track(reg_a);
        let _id_b = handle.register_track(reg_b);

        let b = wait_for_chunks(&mut rx_b, 3, Duration::from_secs(5));
        assert!(
            b >= 3,
            "track B should keep working after track A panics, got {b}"
        );

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_seek_enters_pending_reset() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let source = MockSource::new(100);
        let seek = Arc::clone(&source.seek);
        let (reg, mut rx, _) = make_registration(source, 32, 1);

        let _id = handle.register_track(reg);

        let got = wait_for_chunks(&mut rx, 2, Duration::from_secs(5));
        assert!(got >= 2);

        let _ = seek.begin(Duration::from_secs(10));
        handle.wake();

        thread_sleep(Duration::from_millis(100));

        let after_seek = wait_for_chunks(&mut rx, 1, Duration::from_secs(5));
        assert!(after_seek >= 1, "should resume decoding after seek");

        handle.shutdown();
    }

    #[kithara::test(tokio)]
    async fn worker_preload_gate_fires_on_progress() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg, _rx, preload_gate) = make_registration(MockSource::new(10), 32, 3);

        let _id = handle.register_track(reg);

        platform_timeout(Duration::from_secs(1), preload_gate.wait())
            .await
            .expect("preload gate must open once the preload threshold is met");
        assert!(preload_gate.is_ready());

        handle.shutdown();
    }

    #[kithara::test(tokio)]
    async fn worker_preload_gate_fires_on_eof() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg, _rx, preload_gate) = make_registration(MockSource::new(0), 32, 8);

        let _id = handle.register_track(reg);

        platform_timeout(Duration::from_secs(1), preload_gate.wait())
            .await
            .expect("EOF before the preload threshold must still open the gate");
        assert!(preload_gate.is_ready());

        handle.shutdown();
    }

    #[kithara::test(tokio)]
    async fn worker_preload_gate_fires_on_failure() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg, _rx, preload_gate) = make_registration(FailingSource::default(), 32, 8);

        let _id = handle.register_track(reg);

        platform_timeout(Duration::from_secs(1), preload_gate.wait())
            .await
            .expect("a decoder failure must open the gate so preload never stalls");
        assert!(preload_gate.is_ready());

        handle.shutdown();
    }

    #[kithara::test(tokio)]
    async fn worker_preload_gate_reopens_after_seek() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let source = MockSource::new(10);
        let seek = Arc::clone(&source.seek);
        let (reg, _rx, preload_gate) = make_registration(source, 32, 1);
        let _id = handle.register_track(reg);

        platform_timeout(Duration::from_secs(1), preload_gate.wait())
            .await
            .expect("initial preload gate must open");
        assert!(preload_gate.is_ready());

        let _ = seek.begin(Duration::from_secs(1));
        handle.wake();

        platform_timeout(Duration::from_secs(1), preload_gate.wait())
            .await
            .expect("re-armed preload gate must reopen after the seek refills");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_unregister_removes_track() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg, mut rx, _) = make_registration(MockSource::new(100), 32, 1);

        let id = handle.register_track(reg);

        let got = wait_for_chunks(&mut rx, 2, Duration::from_secs(5));
        assert!(got >= 2);

        handle.unregister_track(id);
        thread_sleep(Duration::from_millis(50));

        while rx.try_pop().is_some() {}

        thread_sleep(Duration::from_millis(50));
        assert!(rx.try_pop().is_none(), "no chunks after unregister");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_service_class_prioritises_audible() {
        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(100), 4, 0);
        let class_a = Arc::clone(&reg_a.service_class);
        let _id_a = handle.register_track(reg_a);

        let (reg_b, mut rx_b, _) = make_registration(MockSource::new(100), 4, 0);
        let class_b = Arc::clone(&reg_b.service_class);
        let _id_b = handle.register_track(reg_b);

        thread_sleep(Duration::from_millis(30));

        while rx_a.try_pop().is_some() {}
        while rx_b.try_pop().is_some() {}

        class_a.store(ServiceClass::Idle);
        class_b.store(ServiceClass::Audible);
        handle.wake();

        thread_sleep(Duration::from_millis(50));

        let got_a = {
            let mut n = 0;
            while rx_a.try_pop().is_some() {
                n += 1;
            }
            n
        };
        let got_b = {
            let mut n = 0;
            while rx_b.try_pop().is_some() {
                n += 1;
            }
            n
        };
        assert!(
            got_b >= got_a,
            "Audible track should get at least as many chunks: A={got_a}, B={got_b}"
        );

        handle.shutdown();
    }

    /// A slow/blocked track must not starve a producing track.
    ///
    /// Reproduces the production bug: HLS track waiting for network data
    /// blocks the shared worker's `step_track()` call, causing MP3 track
    /// audio to stutter.
    ///
    /// The mock simulates a track whose `step_track()` blocks the thread
    /// for 50ms (like a real `wait_range()` call waiting for network data).
    /// The worker must still deliver chunks to the ready track at a
    /// rate sufficient for glitch-free playback.
    #[kithara::test]
    fn shared_worker_blocking_track_does_not_starve_producing_track() {
        struct BlockingSource {
            seek_obs: Arc<dyn SeekObserve>,
            blocking: Arc<AtomicBool>,
        }

        impl AudioWorkerSource for BlockingSource {
            type Chunk = PcmChunk;

            fn step_track(&mut self) -> TrackStep<PcmChunk> {
                if self.blocking.load(Ordering::Relaxed) {
                    thread_sleep(Duration::from_millis(10));
                    TrackStep::Blocked(WaitingReason::Waiting)
                } else {
                    TrackStep::Blocked(WaitingReason::Waiting)
                }
            }

            fn seek_observe(&self) -> Arc<dyn SeekObserve> {
                Arc::clone(&self.seek_obs)
            }
        }

        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(100), 32, 0);
        let _id_a = handle.register_track(reg_a);

        let blocking = Arc::new(AtomicBool::new(true));
        let blocking_source = BlockingSource {
            seek_obs: Timeline::new().seek_observe(),
            blocking: Arc::clone(&blocking),
        };
        let (reg_b, _rx_b, _) = make_registration(blocking_source, 32, 0);
        let _id_b = handle.register_track(reg_b);

        thread_sleep(Duration::from_millis(500));

        let mut got_a = 0;
        while rx_a.try_pop().is_some() {
            got_a += 1;
        }

        assert!(
            got_a >= 11,
            "Producing track must not be starved by blocking track: \
             got only {got_a} chunks in 1s (expected ≥11 for glitch-free)"
        );

        blocking.store(false, Ordering::Relaxed);
        handle.shutdown();
    }

    /// A track that blocks inside `step_track()` (simulating Symphonia read
    /// waiting on network data) must not starve other tracks.
    ///
    /// This is the REAL production bug: HLS decode path enters `wait_range()`
    /// which blocks the entire worker thread. MP3 track's ringbuf drains
    /// during the block, causing audio underrun.
    ///
    /// Target: even with 50ms blocking per HLS step, MP3 track should
    /// still receive enough chunks for glitch-free playback.
    #[kithara::test]
    fn shared_worker_sync_blocking_step_starves_other_tracks() {
        struct SlowDecodeSource {
            seek_obs: Arc<dyn SeekObserve>,
            block_ms: u64,
        }

        impl AudioWorkerSource for SlowDecodeSource {
            type Chunk = PcmChunk;

            fn step_track(&mut self) -> TrackStep<PcmChunk> {
                thread_sleep(Duration::from_millis(self.block_ms));
                TrackStep::Produced(Fetch::new(PcmChunk::default(), false, 0))
            }

            fn seek_observe(&self) -> Arc<dyn SeekObserve> {
                Arc::clone(&self.seek_obs)
            }
        }

        let handle = AudioWorkerHandle::with_cancel(CancellationToken::default());

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(1000), 32, 0);
        let _id_a = handle.register_track(reg_a);

        let slow_source = SlowDecodeSource {
            seek_obs: Timeline::new().seek_observe(),
            block_ms: 10,
        };
        let (reg_b, mut rx_b, _) = make_registration(slow_source, 32, 0);
        let _id_b = handle.register_track(reg_b);

        let mut max_gap = Duration::ZERO;
        let mut last_chunk_time = Instant::now();
        let mut total_chunks = 0u32;
        let deadline = Instant::now() + Duration::from_secs(1);

        while Instant::now() < deadline {
            if rx_a.try_pop().is_some() {
                let gap = last_chunk_time.elapsed();
                if total_chunks > 0 && gap > max_gap {
                    max_gap = gap;
                }
                last_chunk_time = Instant::now();
                total_chunks += 1;
            }
            while rx_b.try_pop().is_some() {}
            thread_sleep(Duration::from_millis(5));
        }

        assert!(
            max_gap < Duration::from_millis(46),
            "Max gap between chunks for fast track: {max_gap:?} (limit 46ms). \
             Slow track's sync blocking causes starvation. \
             Total chunks delivered: {total_chunks}"
        );
    }
}
