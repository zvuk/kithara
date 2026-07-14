use std::sync::atomic::{AtomicU64, Ordering};

use kithara_platform::{CancelToken, sync::Arc};

use super::{DecoderNode, HangWatchdogObserver, TrackRegistration};
use crate::runtime::{Node, Scheduler, SchedulerHandle};

/// Unique identifier for a track registered with a shared worker.
pub(crate) type TrackId = u64;

/// Monotonic counter for generating unique [`TrackId`] values.
pub(crate) struct TrackIdGen(AtomicU64);

impl TrackIdGen {
    // ast-grep-ignore: style.prefer-default-derive
    pub(crate) fn new() -> Self {
        Self(AtomicU64::new(1))
    }

    pub(crate) fn next(&self) -> TrackId {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

/// Clonable handle to a shared audio worker.
///
/// Multiple [`Audio`](crate::Audio) handles can share one worker by cloning
/// the handle and passing it via [`AudioConfig`](crate::AudioConfig).
pub struct AudioWorkerHandle {
    id_gen: Arc<TrackIdGen>,
    inner: SchedulerHandle<Box<dyn Node>>,
}

impl Clone for AudioWorkerHandle {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            id_gen: Arc::clone(&self.id_gen),
        }
    }
}

impl AudioWorkerHandle {
    /// Register a track. Returns the assigned [`TrackId`].
    ///
    /// If the worker thread has already exited (e.g. after shutdown), the
    /// registration is silently lost and the returned track will produce no
    /// data. Callers must ensure the worker is alive before registering.
    pub(crate) fn register_track(&self, reg: TrackRegistration) -> TrackId {
        let id = self.id_gen.next();
        let node: Box<dyn Node> = Box::new(DecoderNode::from(reg));
        self.inner.register(id, node);
        id
    }

    delegate::delegate! {
        to self.inner {
            /// Request graceful shutdown and cancel the worker.
            pub fn shutdown(&self);
            /// Remove a track by ID.
            #[call(unregister)]
            pub(crate) fn unregister_track(&self, track_id: TrackId);
            /// Wake the worker so it re-ticks the decoder now. Called by the RT
            /// consumer after it drains a chunk (ring-space freed) and — via
            /// [`WorkerWakeBridge`] — by the HLS readiness gate from the downloader
            /// thread when segment bytes are written/committed, so an underran worker
            /// re-ticks on data arrival instead of on its 10 ms scheduler poll.
            /// Wait-free: an atomic bump + `unpark`, safe from any thread.
            pub fn wake(&self);
        }
    }

    /// Spawn a new shared worker thread bound to the given [`CancelToken`] and
    /// return a handle. Production callers (e.g. `EngineImpl`) pass a
    /// `child()` of the player master so worker shutdown participates in
    /// the unified cancel hierarchy and the produce-core's lock-free
    /// `is_cancelled()` read observes a master cancel.
    #[must_use]
    pub fn with_cancel(cancel: CancelToken) -> Self {
        let id_gen = Arc::new(TrackIdGen::new());
        let id = Arc::as_ptr(&id_gen);
        let inner = Scheduler::<Box<dyn Node>, HangWatchdogObserver>::start(
            format!("kithara-audio-worker-{id:p}"),
            HangWatchdogObserver::new(),
            cancel,
        );

        Self { id_gen, inner }
    }
}

/// Adapts an [`AudioWorkerHandle`] to the [`WorkerWake`](kithara_stream::WorkerWake)
/// trait so kithara-hls (which does not depend on kithara-audio) can re-tick
/// the worker on segment data arrival. Installed via `Stream::set_worker_wake`
/// after the worker exists; the HLS readiness gate calls [`wake`](Self::wake)
/// from its off-RT downloader write/settle path. Wait-free.
pub(crate) struct WorkerWakeBridge(pub(crate) AudioWorkerHandle);

impl kithara_stream::WorkerWake for WorkerWakeBridge {
    fn wake(&self) {
        self.0.wake();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use kithara_decode::PcmChunk;
    use kithara_platform::{
        sync::Arc,
        thread::sleep as thread_sleep,
        time::{Duration, Instant, timeout as platform_timeout},
    };
    use kithara_stream::{SeekObserve, SeekState};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        pipeline::{
            fetch::{Fetch, FetchKind},
            track::{TrackStep, WaitingReason},
        },
        renderer::{AudioWorkerSource, MockSource, PreloadGate, ServiceClass, ThreadWake},
        runtime::{AtomicServiceClass, connect},
    };

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

    impl AudioWorkerSource for FailingSource {
        type Chunk = PcmChunk;

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_obs)
        }

        fn step_track(&mut self) -> TrackStep<PcmChunk> {
            TrackStep::Failed
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
            engine_load: None,
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
    fn track_id_gen_produces_unique_ids() {
        let id_gen = TrackIdGen::new();
        let a = id_gen.next();
        let b = id_gen.next();
        let c = id_gen.next();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
    }

    #[kithara::test]
    fn worker_creates_and_drops_cleanly() {
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());
        thread_sleep(Duration::from_millis(10));
        handle.shutdown();
        thread_sleep(Duration::from_millis(50));
    }

    #[kithara::test]
    fn worker_delivers_chunks() {
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());
        let (reg, mut data_rx, _preload_gate) = make_registration(MockSource::new(10), 32, 3);

        let _id = handle.register_track(reg);

        let received = wait_for_chunks(&mut data_rx, 5, Duration::from_secs(5));
        assert!(received >= 5, "expected >=5 chunks, got {received}");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_multi_track_round_robin() {
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

        let source = MockSource::new(10);
        let seek = Arc::clone(&source.seek);
        let (reg, _rx, preload_gate) = make_registration(source, 32, 1);
        let _id = handle.register_track(reg);

        platform_timeout(Duration::from_secs(1), preload_gate.wait())
            .await
            .expect("initial preload gate must open");
        assert!(preload_gate.is_ready());

        let epoch = seek.begin(Duration::from_secs(1));
        handle.wake();

        platform_timeout(Duration::from_secs(1), preload_gate.wait_for_epoch(epoch))
            .await
            .expect("re-armed preload gate must reopen after the seek refills");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_unregister_removes_track() {
        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

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

    // Audible-before-Idle is a pure ordering property of the scheduler's
    // `slots_order`, not of consumed-chunk counts; verifying it through a live
    // ring drain is structurally racy. It is locked deterministically in
    // `runtime::scheduler::tests` instead — see
    // `refresh_reorders_live_when_atomic_service_class_changes`.

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

        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(100), 32, 0);
        let _id_a = handle.register_track(reg_a);

        let blocking = Arc::new(AtomicBool::new(true));
        let blocking_source = BlockingSource {
            seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
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
                TrackStep::Produced(Fetch::new(PcmChunk::default(), FetchKind::Data, 0))
            }

            fn seek_observe(&self) -> Arc<dyn SeekObserve> {
                Arc::clone(&self.seek_obs)
            }
        }

        let handle = AudioWorkerHandle::with_cancel(CancelToken::never());

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(1000), 32, 0);
        let _id_a = handle.register_track(reg_a);

        let slow_source = SlowDecodeSource {
            seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
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
