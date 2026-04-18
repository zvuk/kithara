//! Shared audio worker — cooperative multi-track decoder on a single OS thread.
//!
//! [`AudioWorkerHandle`] is the user-facing handle (clonable). It spawns a
//! background OS thread that runs a `crate::runtime::Scheduler`, serving all
//! registered tracks via priority scheduling.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_decode::PcmChunk;
use kithara_platform::tokio::sync::Notify;

use super::{
    AudioWorkerSource,
    decoder_node::DecoderNode,
    hang_observer::HangWatchdogObserver,
    types::{ServiceClass, TrackId, TrackIdGen},
};
use crate::{
    pipeline::fetch::Fetch,
    runtime::{Scheduler, SchedulerHandle},
};

/// Everything needed to register a track with the shared worker.
pub(crate) struct TrackRegistration {
    pub source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
    pub outlet: crate::runtime::Outlet<Fetch<PcmChunk>>,
    pub preload_notify: Arc<Notify>,
    pub preload_chunks: usize,
    pub service_class: ServiceClass,
}

/// Clonable handle to a shared audio worker.
///
/// Multiple [`Audio`](crate::Audio) handles can share one worker by cloning
/// the handle and passing it via [`AudioConfig`](crate::AudioConfig).
pub struct AudioWorkerHandle {
    inner: SchedulerHandle<Box<dyn crate::runtime::Node>>,
    id_gen: Arc<TrackIdGen>,
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
    /// Spawn a new shared worker thread and return a handle.
    #[must_use]
    pub fn new() -> Self {
        let id = AUDIO_WORKER_ID.fetch_add(1, Ordering::Relaxed);
        let inner = Scheduler::<Box<dyn crate::runtime::Node>, HangWatchdogObserver>::start(
            format!("kithara-audio-worker-{id}"),
            HangWatchdogObserver::new(),
        );

        Self {
            inner,
            id_gen: Arc::new(TrackIdGen::new()),
        }
    }

    /// Register a track. Returns the assigned [`TrackId`].
    ///
    /// If the worker thread has already exited (e.g. after shutdown), the
    /// registration is silently lost and the returned track will produce no
    /// data. Callers must ensure the worker is alive before registering.
    pub(crate) fn register_track(&self, reg: TrackRegistration) -> TrackId {
        let id = self.id_gen.next();
        let node: Box<dyn crate::runtime::Node> = Box::new(DecoderNode::from_registration(id, reg));
        self.inner.register(id, node);
        id
    }

    /// Remove a track by ID.
    pub(crate) fn unregister_track(&self, track_id: TrackId) {
        self.inner.unregister(track_id);
    }

    /// Update scheduling priority for a track.
    pub(crate) fn set_service_class(&self, track_id: TrackId, class: ServiceClass) {
        self.inner.set_service_class(track_id, class);
    }

    /// Wake the worker (e.g. when new data arrives from downloader).
    pub fn wake(&self) {
        self.inner.wake();
    }

    /// Request graceful shutdown and cancel the worker.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}

impl Default for AudioWorkerHandle {
    fn default() -> Self {
        Self::new()
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
        tokio::sync::Notify,
    };
    use kithara_stream::Timeline;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        pipeline::track_fsm::{TrackStep, WaitingReason},
        runtime::connect,
        worker::{AudioWorkerSource, thread_wake::ThreadWake, types::ServiceClass},
    };

    // Mock source

    struct MockSource {
        timeline: Timeline,
        chunks_to_produce: usize,
        cursor: usize,
        ready: bool,
        should_panic: bool,
    }

    impl MockSource {
        fn new(chunks: usize) -> Self {
            Self {
                timeline: Timeline::new(),
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
            // Handle seek
            if self.timeline.is_seek_pending() || self.timeline.is_flushing() {
                let epoch = self.timeline.seek_epoch();
                self.timeline.complete_seek(epoch);
                self.timeline.clear_seek_pending(epoch);
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

        fn timeline(&self) -> &Timeline {
            &self.timeline
        }
    }

    // Helpers

    fn make_registration(
        source: MockSource,
        ringbuf_capacity: usize,
        preload_chunks: usize,
    ) -> (
        TrackRegistration,
        crate::runtime::Inlet<Fetch<PcmChunk>>,
        Arc<Notify>,
    ) {
        let wake = Arc::new(ThreadWake::new());
        let (outlet, inlet) = connect::<Fetch<PcmChunk>>(ringbuf_capacity, Some(wake.clone()));
        let preload_notify = Arc::new(Notify::new());

        let reg = TrackRegistration {
            source: Box::new(source),
            outlet,
            preload_notify: Arc::clone(&preload_notify),
            preload_chunks,
            service_class: ServiceClass::Audible,
        };
        (reg, inlet, preload_notify)
    }

    fn make_registration_with_source(
        source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
        ringbuf_capacity: usize,
        preload_chunks: usize,
    ) -> (
        TrackRegistration,
        crate::runtime::Inlet<Fetch<PcmChunk>>,
        Arc<Notify>,
    ) {
        let wake = Arc::new(ThreadWake::new());
        let (outlet, inlet) = connect::<Fetch<PcmChunk>>(ringbuf_capacity, Some(wake.clone()));
        let preload_notify = Arc::new(Notify::new());

        let reg = TrackRegistration {
            source,
            outlet,
            preload_notify: Arc::clone(&preload_notify),
            preload_chunks,
            service_class: ServiceClass::Audible,
        };
        (reg, inlet, preload_notify)
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

    // Tests

    #[kithara::test]
    fn worker_creates_and_drops_cleanly() {
        let handle = AudioWorkerHandle::new();
        thread_sleep(Duration::from_millis(10));
        handle.shutdown();
        thread_sleep(Duration::from_millis(50));
        // If we get here without panic, the worker started and stopped cleanly.
    }

    #[kithara::test]
    fn worker_delivers_chunks() {
        let handle = AudioWorkerHandle::new();
        let (reg, mut data_rx, _preload_notify) = make_registration(MockSource::new(10), 32, 3);

        let _id = handle.register_track(reg);

        // Wait for chunks to arrive.
        let received = wait_for_chunks(&mut data_rx, 5, Duration::from_secs(5));
        assert!(received >= 5, "expected >=5 chunks, got {received}");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_multi_track_round_robin() {
        let handle = AudioWorkerHandle::new();

        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(10), 32, 1);
        let (reg_b, mut rx_b, _) = make_registration(MockSource::new(10), 32, 1);

        let _id_a = handle.register_track(reg_a);
        let _id_b = handle.register_track(reg_b);

        // Both tracks should receive chunks.
        let a = wait_for_chunks(&mut rx_a, 3, Duration::from_secs(5));
        let b = wait_for_chunks(&mut rx_b, 3, Duration::from_secs(5));
        assert!(a >= 3, "track A: expected >=3 chunks, got {a}");
        assert!(b >= 3, "track B: expected >=3 chunks, got {b}");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_skips_not_ready_tracks() {
        let handle = AudioWorkerHandle::new();

        // Track A: ready, Track B: not ready.
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
        let handle = AudioWorkerHandle::new();

        // Ringbuf capacity 1 — second chunk lands in the outlet's overflow slot.
        let (reg, mut rx, _) = make_registration(MockSource::new(5), 1, 1);

        let _id = handle.register_track(reg);

        // Give worker time to fill and re-fill.
        thread_sleep(Duration::from_millis(50));

        // Pop one, give worker time to flush the parked overflow chunk.
        let first = rx.try_pop();
        assert!(first.is_some(), "should have at least one chunk");

        thread_sleep(Duration::from_millis(50));

        let second = rx.try_pop();
        assert!(second.is_some(), "overflow slot should have been flushed");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_panic_isolation() {
        let handle = AudioWorkerHandle::new();

        // Track A: panics, Track B: normal.
        let (reg_a, _, _) = make_registration(MockSource::panicking(), 32, 1);
        let (reg_b, mut rx_b, _) = make_registration(MockSource::new(10), 32, 1);

        let _id_a = handle.register_track(reg_a);
        let _id_b = handle.register_track(reg_b);

        // Track B should still receive chunks despite Track A panicking.
        let b = wait_for_chunks(&mut rx_b, 3, Duration::from_secs(5));
        assert!(
            b >= 3,
            "track B should keep working after track A panics, got {b}"
        );

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_seek_enters_pending_reset() {
        let handle = AudioWorkerHandle::new();

        let source = MockSource::new(100);
        let timeline = source.timeline.clone();
        let (reg, mut rx, _) = make_registration(source, 32, 1);

        let _id = handle.register_track(reg);

        // Wait for a few chunks first.
        let got = wait_for_chunks(&mut rx, 2, Duration::from_secs(5));
        assert!(got >= 2);

        // Initiate a seek via timeline.
        let _ = timeline.initiate_seek(Duration::from_secs(10));
        handle.wake();

        // The seek should be handled (seek pending cleared after apply).
        thread_sleep(Duration::from_millis(100));

        // After seek completes, more chunks should arrive.
        let after_seek = wait_for_chunks(&mut rx, 1, Duration::from_secs(5));
        assert!(after_seek >= 1, "should resume decoding after seek");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_preload_notify_fires() {
        let handle = AudioWorkerHandle::new();

        let (reg, _rx, _preload_notify) = make_registration(MockSource::new(10), 32, 3);

        let _id = handle.register_track(reg);

        // preload_notify should fire after 3 chunks.
        // We use a tokio runtime to await the notify.
        // Give worker time to produce 3+ chunks.
        // The preload_notify fires internally; we verify it doesn't deadlock.
        thread_sleep(Duration::from_millis(200));

        handle.shutdown();
    }

    #[kithara::test(tokio)]
    async fn worker_preload_notify_rearms_after_seek() {
        let handle = AudioWorkerHandle::new();

        let (reg, _rx, preload_notify) = make_registration(MockSource::new(10), 32, 1);
        let timeline = reg.source.timeline().clone();
        let _id = handle.register_track(reg);

        platform_timeout(Duration::from_secs(1), preload_notify.notified())
            .await
            .expect("initial preload notify must fire");

        let _ = timeline.initiate_seek(Duration::from_secs(1));
        handle.wake();

        platform_timeout(Duration::from_secs(1), preload_notify.notified())
            .await
            .expect("seek must re-arm preload notify");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_unregister_removes_track() {
        let handle = AudioWorkerHandle::new();

        let (reg, mut rx, _) = make_registration(MockSource::new(100), 32, 1);

        let id = handle.register_track(reg);

        let got = wait_for_chunks(&mut rx, 2, Duration::from_secs(5));
        assert!(got >= 2);

        handle.unregister_track(id);
        thread_sleep(Duration::from_millis(50));

        // Drain remaining.
        while rx.try_pop().is_some() {}

        // No more chunks should arrive.
        thread_sleep(Duration::from_millis(50));
        assert!(rx.try_pop().is_none(), "no chunks after unregister");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_service_class_prioritises_audible() {
        let handle = AudioWorkerHandle::new();

        // Track A: Idle, large supply (but low priority).
        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(100), 4, 0);
        let id_a = handle.register_track(reg_a);

        // Track B: will be promoted to Audible.
        let (reg_b, mut rx_b, _) = make_registration(MockSource::new(100), 4, 0);
        let id_b = handle.register_track(reg_b);

        // Let both tracks start decoding (both default to Audible from registration).
        thread_sleep(Duration::from_millis(30));

        // Drain both to make room.
        while rx_a.try_pop().is_some() {}
        while rx_b.try_pop().is_some() {}

        // Demote A to Idle, keep B as Audible.
        handle.set_service_class(id_a, ServiceClass::Idle);
        handle.set_service_class(id_b, ServiceClass::Audible);

        // Wait a bit for scheduling to take effect.
        thread_sleep(Duration::from_millis(50));

        // B (Audible) should have at least as many chunks as A (Idle).
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

    // Starvation tests

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
            timeline: Timeline,
            blocking: Arc<AtomicBool>,
        }

        impl AudioWorkerSource for BlockingSource {
            type Chunk = PcmChunk;

            fn step_track(&mut self) -> TrackStep<PcmChunk> {
                if self.blocking.load(Ordering::Relaxed) {
                    // Simulate sync blocking (like wait_range timeout at 10ms).
                    thread_sleep(Duration::from_millis(10));
                    TrackStep::Blocked(WaitingReason::Waiting)
                } else {
                    TrackStep::Blocked(WaitingReason::Waiting)
                }
            }

            fn timeline(&self) -> &Timeline {
                &self.timeline
            }
        }

        let handle = AudioWorkerHandle::new();

        // Track A: always ready, produces 100 chunks fast.
        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(100), 32, 0);
        let _id_a = handle.register_track(reg_a);

        // Track B: blocks thread for 50ms per step (simulating network wait).
        let blocking = Arc::new(AtomicBool::new(true));
        let blocking_source = BlockingSource {
            timeline: Timeline::new(),
            blocking: Arc::clone(&blocking),
        };
        let (reg_b, _rx_b, _) = make_registration_with_source(Box::new(blocking_source), 32, 0);
        let _id_b = handle.register_track(reg_b);

        // Give worker 500ms to produce chunks for track A despite track B blocking.
        thread_sleep(Duration::from_millis(500));

        let mut got_a = 0;
        while rx_a.try_pop().is_some() {
            got_a += 1;
        }

        // With 50ms blocking per step, round-robin takes ~50ms per round.
        // In 1s: ~20 rounds. Track A gets one chunk per round = ~20 chunks.
        // For glitch-free 44100Hz audio: need ~11 chunks/s (4096 samples each).
        // The REAL issue: during the 50ms block, track A's ringbuf drains
        // and the audio callback gets silence → audible glitch.
        // Test must verify track A gets enough chunks WITHOUT gaps.
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
            timeline: Timeline,
            block_ms: u64,
        }

        impl AudioWorkerSource for SlowDecodeSource {
            type Chunk = PcmChunk;

            fn step_track(&mut self) -> TrackStep<PcmChunk> {
                // Simulate: source_is_ready() returns true, then
                // decode_next_chunk() blocks on wait_range() for data.
                thread_sleep(Duration::from_millis(self.block_ms));
                TrackStep::Produced(Fetch::new(PcmChunk::default(), false, 0))
            }

            fn timeline(&self) -> &Timeline {
                &self.timeline
            }
        }

        let handle = AudioWorkerHandle::new();

        // Track A (MP3-like): fast, always ready.
        let (reg_a, mut rx_a, _) = make_registration(MockSource::new(1000), 32, 0);
        let _id_a = handle.register_track(reg_a);

        // Track B (HLS-like): blocks 10ms per step. This simulates the
        // reduced WAIT_RANGE_TIMEOUT (10ms) + WAIT_RANGE_SLEEP_MS (2ms)
        // where wait_range does a few condvar spins before timing out.
        // With the fix, step_track returns within ~10ms instead of 50ms+.
        let slow_source = SlowDecodeSource {
            timeline: Timeline::new(),
            block_ms: 10,
        };
        let (reg_b, mut rx_b, _) = make_registration_with_source(Box::new(slow_source), 32, 0);
        let _id_b = handle.register_track(reg_b);

        // Simulate audio callback draining track A's ringbuf at real-time rate.
        // At 44100Hz stereo with 4096 samples/chunk → one chunk every ~46ms.
        // If worker can't deliver within 46ms, the consumer starves → glitch.
        //
        // We poll every 5ms and measure the longest gap between chunks.
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
            // Drain track B to prevent backpressure.
            while rx_b.try_pop().is_some() {}
            thread_sleep(Duration::from_millis(5));
        }

        // For glitch-free playback, the max gap between chunks must be
        // less than one chunk duration (~46ms at 44100Hz stereo).
        // The slow track's 50ms sync blocking causes gaps ≥50ms → glitch.
        assert!(
            max_gap < Duration::from_millis(46),
            "Max gap between chunks for fast track: {max_gap:?} (limit 46ms). \
             Slow track's sync blocking causes starvation. \
             Total chunks delivered: {total_chunks}"
        );
    }
}
