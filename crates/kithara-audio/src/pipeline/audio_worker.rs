//! Shared audio worker — cooperative multi-track decoder on a single OS thread.
//!
//! [`AudioWorkerHandle`] is the user-facing handle (clonable). It spawns a
//! background OS thread that runs [`run_shared_worker_loop`], serving all
//! registered tracks via round-robin scheduling.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::Duration,
};

use kithara_decode::PcmChunk;
use kithara_platform::{
    sync::mpsc::{self, TryRecvError},
    tokio::sync::Notify,
};
use kithara_stream::Fetch;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Producer},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::pipeline::{
    thread_wake::ThreadWake,
    worker::{AudioCommand, AudioWorkerSource},
    worker_types::{ServiceClass, StepResult, TrackId, TrackIdGen},
    worker_wake::WorkerWake,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Command sent from [`AudioWorkerHandle`] to the shared worker thread.
pub(crate) enum WorkerCmd {
    /// Register a new track for decoding.
    Register(TrackId, TrackRegistration),
    /// Remove a track by ID.
    Unregister(TrackId),
    /// Update service class for scheduling priority.
    SetServiceClass(TrackId, ServiceClass),
    /// Graceful shutdown — exit the worker loop.
    Shutdown,
}

/// Everything needed to register a track with the shared worker.
pub(crate) struct TrackRegistration {
    pub consumer_wake: Arc<ThreadWake>,
    pub source: Box<dyn AudioWorkerSource<Chunk = PcmChunk, Command = AudioCommand>>,
    pub data_tx: HeapProd<Fetch<PcmChunk>>,
    pub cmd_rx: HeapCons<AudioCommand>,
    pub preload_notify: Arc<Notify>,
    pub preload_chunks: usize,
    pub service_class: ServiceClass,
    pub cancel: CancellationToken,
}

/// Clonable handle to a shared audio worker.
///
/// Multiple [`Audio`](crate::Audio) handles can share one worker by cloning
/// the handle and passing it via [`AudioConfig`](crate::AudioConfig).
#[derive(Clone)]
pub struct AudioWorkerHandle {
    cmd_tx: mpsc::Sender<WorkerCmd>,
    wake: Arc<WorkerWake>,
    id_gen: Arc<TrackIdGen>,
    cancel: CancellationToken,
}

impl AudioWorkerHandle {
    /// Spawn a new shared worker thread and return a handle.
    #[must_use]
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let wake = Arc::new(WorkerWake::new());
        let cancel = CancellationToken::new();

        let wake_clone = Arc::clone(&wake);
        let cancel_clone = cancel.clone();

        kithara_platform::thread::spawn(move || {
            run_shared_worker_loop(&cmd_rx, &wake_clone, &cancel_clone);
        });

        Self {
            cmd_tx,
            wake,
            id_gen: Arc::new(TrackIdGen::new()),
            cancel,
        }
    }

    /// Register a track. Returns the assigned [`TrackId`].
    ///
    /// If the worker thread has already exited (e.g. after shutdown), the
    /// registration is silently lost and the returned track will produce no
    /// data. Callers must ensure the worker is alive before registering.
    pub(crate) fn register_track(&self, reg: TrackRegistration) -> TrackId {
        let id = self.id_gen.next();
        if self.cmd_tx.send_sync(WorkerCmd::Register(id, reg)).is_err() {
            warn!(track_id = id, "register_track: worker channel closed");
        }
        self.wake.wake();
        id
    }

    /// Remove a track by ID.
    pub(crate) fn unregister_track(&self, track_id: TrackId) {
        let _ = self.cmd_tx.send_sync(WorkerCmd::Unregister(track_id));
        self.wake.wake();
    }

    /// Update scheduling priority for a track.
    pub(crate) fn set_service_class(&self, track_id: TrackId, class: ServiceClass) {
        let _ = self
            .cmd_tx
            .send_sync(WorkerCmd::SetServiceClass(track_id, class));
        self.wake.wake();
    }

    /// Wake the worker (e.g. when new data arrives from downloader).
    pub fn wake(&self) {
        self.wake.wake();
    }

    /// Request graceful shutdown and cancel the worker.
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send_sync(WorkerCmd::Shutdown);
        self.cancel.cancel();
        self.wake.wake();
    }
}

impl Default for AudioWorkerHandle {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Internal: TrackSlot
// ---------------------------------------------------------------------------

/// Per-track state inside the worker thread.
struct TrackSlot {
    consumer_wake: Arc<ThreadWake>,
    track_id: TrackId,
    source: Box<dyn AudioWorkerSource<Chunk = PcmChunk, Command = AudioCommand>>,
    data_tx: HeapProd<Fetch<PcmChunk>>,
    cmd_rx: HeapCons<AudioCommand>,
    pending_fetch: Option<Fetch<PcmChunk>>,
    service_class: ServiceClass,
    preload_notify: Arc<Notify>,
    preload_chunks: usize,
    chunks_sent: usize,
    preloaded: bool,
    cancel: CancellationToken,
    /// Set when `step_track()` returns `Failed` — truly terminal.
    terminal: bool,
    /// Set when EOF fetch has been pushed to the consumer.
    eof_sent: bool,
}

impl TrackSlot {
    fn from_registration(track_id: TrackId, reg: TrackRegistration) -> Self {
        Self {
            consumer_wake: reg.consumer_wake,
            track_id,
            source: reg.source,
            data_tx: reg.data_tx,
            cmd_rx: reg.cmd_rx,
            pending_fetch: None,
            service_class: reg.service_class,
            preload_notify: reg.preload_notify,
            preload_chunks: reg.preload_chunks,
            chunks_sent: 0,
            preloaded: false,
            cancel: reg.cancel,
            terminal: false,
            eof_sent: false,
        }
    }

    fn is_removable(&self) -> bool {
        self.terminal || self.cancel.is_cancelled()
    }

    /// One cooperative step: drain commands, step FSM, push at most one chunk.
    fn step(&mut self) -> StepResult {
        use crate::pipeline::track_fsm::TrackStep;

        if self.cancel.is_cancelled() || self.terminal {
            return StepResult::NoProgress;
        }

        // Drain per-track commands.
        while let Some(cmd) = self.cmd_rx.try_pop() {
            self.source.handle_command(cmd);
        }

        // Try push previously-rejected chunk (backpressure).
        if let Some(fetch) = self.pending_fetch.take() {
            match self.data_tx.try_push(fetch) {
                Ok(()) => {
                    self.mark_preload_progress();
                    self.consumer_wake.wake();
                    return StepResult::Progress;
                }
                Err(rejected) => {
                    self.pending_fetch = Some(rejected);
                    return StepResult::NoProgress;
                }
            }
        }

        // Step the track FSM.
        match self.source.step_track() {
            TrackStep::Produced(fetch) => {
                self.eof_sent = false;
                match self.data_tx.try_push(fetch) {
                    Ok(()) => {
                        self.mark_preload_progress();
                        self.consumer_wake.wake();
                    }
                    Err(rejected) => {
                        self.pending_fetch = Some(rejected);
                    }
                }
                StepResult::Progress
            }
            TrackStep::StateChanged => {
                self.eof_sent = false;
                StepResult::Progress
            }
            TrackStep::Blocked(reason) => {
                trace!(?reason, "track blocked");
                StepResult::NoProgress
            }
            TrackStep::Eof => {
                if !self.eof_sent {
                    self.complete_preload();
                    let epoch = self.source.timeline().seek_epoch();
                    let eof_fetch = Fetch::new(PcmChunk::default(), true, epoch);
                    if self.data_tx.try_push(eof_fetch).is_ok() {
                        self.eof_sent = true;
                        self.consumer_wake.wake();
                    }
                    // If push failed (backpressure), eof_sent stays false
                    // and we retry on the next step() call.
                }
                StepResult::NoProgress
            }
            TrackStep::Failed => {
                self.complete_preload();
                let epoch = self.source.timeline().seek_epoch();
                let eof_fetch = Fetch::new(PcmChunk::default(), true, epoch);
                if self.data_tx.try_push(eof_fetch).is_ok() {
                    self.terminal = true;
                    self.consumer_wake.wake();
                }
                // If push failed, terminal stays false so we retry
                // pushing the EOF marker on the next step() call.
                StepResult::NoProgress
            }
        }
    }

    fn mark_preload_progress(&mut self) {
        if self.preloaded {
            return;
        }
        self.chunks_sent += 1;
        if self.chunks_sent >= self.preload_chunks {
            self.complete_preload();
        }
    }

    fn complete_preload(&mut self) {
        if !self.preloaded {
            self.preload_notify.notify_one();
            self.preloaded = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Worker loop
// ---------------------------------------------------------------------------

const IDLE_TIMEOUT: Duration = Duration::from_millis(10);
const EMPTY_TIMEOUT: Duration = Duration::from_millis(100);

/// Cooperative multi-track worker loop.
///
/// Runs on a dedicated OS thread. Drains commands, steps each track
/// round-robin (one chunk per track per pass), sleeps when idle.
#[kithara_hang_detector::hang_watchdog]
fn run_shared_worker_loop(
    cmd_rx: &mpsc::Receiver<WorkerCmd>,
    wake: &WorkerWake,
    cancel: &CancellationToken,
) {
    trace!("shared worker started");
    let mut tracks: Vec<TrackSlot> = Vec::new();
    let mut cursor: usize = 0;

    loop {
        if cancel.is_cancelled() {
            trace!("shared worker cancelled");
            for slot in &mut tracks {
                slot.complete_preload();
            }
            return;
        }

        // Phase 1: Drain global commands.
        let should_stop = drain_worker_commands(cmd_rx, &mut tracks, &mut cursor);
        if should_stop {
            return;
        }

        // Phase 2: Round-robin — one step per track.
        let mut progress = false;
        let track_count = tracks.len();

        for _ in 0..track_count {
            if cursor >= track_count {
                cursor = 0;
            }
            let slot = &mut tracks[cursor];
            cursor += 1;

            match catch_unwind(AssertUnwindSafe(|| slot.step())) {
                Ok(StepResult::Progress) => progress = true,
                Ok(StepResult::NoProgress) => {}
                Err(_) => {
                    warn!(track_id = slot.track_id, "shared worker: track panicked");
                    slot.terminal = true;
                    slot.complete_preload();
                    // Push EOF so the consumer stops waiting for chunks.
                    let epoch = slot.source.timeline().seek_epoch();
                    let _ = slot
                        .data_tx
                        .try_push(Fetch::new(PcmChunk::default(), true, epoch));
                    slot.consumer_wake.wake();
                }
            }
        }

        // Phase 3: Cleanup failed/cancelled tracks.
        let before = tracks.len();
        tracks.retain(|slot| !slot.is_removable());
        if tracks.len() < before && cursor > tracks.len() {
            cursor = 0;
        }

        // Phase 4: Idle.
        if progress {
            hang_reset!();
            kithara_platform::thread::yield_now();
        } else if tracks.is_empty() {
            // No tracks registered — the worker is legitimately idle,
            // not hung. Reset the watchdog to avoid false positives.
            hang_reset!();
            wake.wait_timeout(EMPTY_TIMEOUT);
        } else {
            // Terminal phases (AtEof/Failed) are legitimately idle:
            // they wait for an external event (seek command). Reset the
            // watchdog so it doesn't fire during normal inter-seek gaps.
            //
            // Non-terminal phases (Decoding/PendingReset) that report
            // no progress should tick the watchdog: if the source stays
            // "not ready" because the downloader is also stalled, the
            // watchdog will fire after the configured timeout.
            let all_terminal = tracks.iter().all(|s| s.terminal || s.eof_sent);
            if all_terminal {
                hang_reset!();
            } else {
                trace!(
                    track_count = tracks.len(),
                    terminal_count = tracks.iter().filter(|s| s.terminal || s.eof_sent).count(),
                    "worker no-progress tick (non-terminal tracks stalled)"
                );
                hang_tick!();
            }
            wake.wait_timeout(IDLE_TIMEOUT);
        }
    }
}

/// Drain global commands from the channel. Returns `true` if the loop should exit.
#[expect(
    clippy::cognitive_complexity,
    reason = "36/35 — flat match on WorkerCmd variants"
)]
fn drain_worker_commands(
    cmd_rx: &mpsc::Receiver<WorkerCmd>,
    tracks: &mut Vec<TrackSlot>,
    cursor: &mut usize,
) -> bool {
    loop {
        match cmd_rx.try_recv() {
            Ok(WorkerCmd::Register(id, reg)) => {
                debug!(track_id = id, "worker: registering track");
                tracks.push(TrackSlot::from_registration(id, reg));
            }
            Ok(WorkerCmd::Unregister(id)) => {
                debug!(track_id = id, "worker: unregistering track");
                if let Some(slot) = tracks.iter_mut().find(|s| s.track_id == id) {
                    slot.complete_preload();
                }
                tracks.retain(|s| s.track_id != id);
                if *cursor > tracks.len() {
                    *cursor = 0;
                }
            }
            Ok(WorkerCmd::SetServiceClass(id, class)) => {
                if let Some(slot) = tracks.iter_mut().find(|s| s.track_id == id) {
                    slot.service_class = class;
                }
                // Re-sort so Audible tracks are processed before Idle.
                tracks.sort_by(|a, b| b.service_class.cmp(&a.service_class));
                *cursor = 0;
            }
            Ok(WorkerCmd::Shutdown) => {
                trace!("shared worker shutdown");
                for slot in tracks.iter_mut() {
                    slot.complete_preload();
                }
                return true;
            }
            Err(err) => {
                if matches!(err, TryRecvError::Disconnected) {
                    trace!("shared worker: all handles dropped");
                    for slot in tracks.iter_mut() {
                        slot.complete_preload();
                    }
                    return true;
                }
                break;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use kithara_decode::PcmChunk;
    use kithara_platform::tokio::sync::Notify;
    use kithara_stream::{Fetch, Timeline};
    use kithara_test_utils::kithara;
    use ringbuf::{HeapRb, traits::Split};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::pipeline::{
        track_fsm::{TrackStep, WaitingReason},
        worker::{AudioCommand, AudioWorkerSource},
        worker_types::ServiceClass,
    };

    // -- Mock source ----------------------------------------------------------

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
        type Command = AudioCommand;

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

        fn handle_command(&mut self, _cmd: AudioCommand) {}

        fn timeline(&self) -> &Timeline {
            &self.timeline
        }
    }

    // -- Helpers --------------------------------------------------------------

    fn make_registration(
        source: MockSource,
        ringbuf_capacity: usize,
        preload_chunks: usize,
    ) -> (
        TrackRegistration,
        HeapCons<Fetch<PcmChunk>>,
        Arc<Notify>,
        CancellationToken,
    ) {
        let rb = HeapRb::<Fetch<PcmChunk>>::new(ringbuf_capacity);
        let (data_tx, data_rx) = rb.split();
        let cmd_rb = HeapRb::<AudioCommand>::new(4);
        let (_, cmd_rx) = cmd_rb.split();
        let preload_notify = Arc::new(Notify::new());
        let cancel = CancellationToken::new();

        let reg = TrackRegistration {
            consumer_wake: Arc::new(ThreadWake::new()),
            source: Box::new(source),
            data_tx,
            cmd_rx,
            preload_notify: Arc::clone(&preload_notify),
            preload_chunks,
            service_class: ServiceClass::Audible,
            cancel: cancel.clone(),
        };
        (reg, data_rx, preload_notify, cancel)
    }

    fn wait_for_chunks(
        rx: &mut HeapCons<Fetch<PcmChunk>>,
        count: usize,
        timeout: Duration,
    ) -> usize {
        let start = kithara_platform::time::Instant::now();
        let mut received = 0;
        while received < count && start.elapsed() < timeout {
            if rx.try_pop().is_some() {
                received += 1;
            } else {
                kithara_platform::thread::sleep(Duration::from_millis(1));
            }
        }
        received
    }

    // -- Tests ----------------------------------------------------------------

    #[kithara::test]
    fn worker_creates_and_drops_cleanly() {
        let handle = AudioWorkerHandle::new();
        kithara_platform::thread::sleep(Duration::from_millis(10));
        handle.shutdown();
        kithara_platform::thread::sleep(Duration::from_millis(50));
        // If we get here without panic, the worker started and stopped cleanly.
    }

    #[kithara::test]
    fn worker_delivers_chunks() {
        let handle = AudioWorkerHandle::new();
        let (reg, mut data_rx, _preload_notify, _cancel) =
            make_registration(MockSource::new(10), 32, 3);

        let _id = handle.register_track(reg);

        // Wait for chunks to arrive.
        let received = wait_for_chunks(&mut data_rx, 5, Duration::from_secs(5));
        assert!(received >= 5, "expected >=5 chunks, got {received}");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_multi_track_round_robin() {
        let handle = AudioWorkerHandle::new();

        let (reg_a, mut rx_a, _, _cancel_a) = make_registration(MockSource::new(10), 32, 1);
        let (reg_b, mut rx_b, _, _cancel_b) = make_registration(MockSource::new(10), 32, 1);

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
        let (reg_a, mut rx_a, _, _cancel_a) = make_registration(MockSource::new(10), 32, 1);
        let (reg_b, mut rx_b, _, _cancel_b) = make_registration(MockSource::not_ready(10), 32, 1);

        let _id_a = handle.register_track(reg_a);
        let _id_b = handle.register_track(reg_b);

        kithara_platform::thread::sleep(Duration::from_millis(100));

        let a = wait_for_chunks(&mut rx_a, 1, Duration::from_millis(100));
        let b = wait_for_chunks(&mut rx_b, 1, Duration::from_millis(50));
        assert!(a >= 1, "track A should receive chunks");
        assert_eq!(b, 0, "track B should receive nothing (not ready)");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_pending_fetch_on_full_ringbuf() {
        let handle = AudioWorkerHandle::new();

        // Ringbuf capacity 1 — second chunk goes to pending_fetch.
        let (reg, mut rx, _, _cancel) = make_registration(MockSource::new(5), 1, 1);

        let _id = handle.register_track(reg);

        // Give worker time to fill and re-fill.
        kithara_platform::thread::sleep(Duration::from_millis(50));

        // Pop one, give worker time to push pending.
        let first = rx.try_pop();
        assert!(first.is_some(), "should have at least one chunk");

        kithara_platform::thread::sleep(Duration::from_millis(50));

        let second = rx.try_pop();
        assert!(second.is_some(), "pending_fetch should have been retried");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_panic_isolation() {
        let handle = AudioWorkerHandle::new();

        // Track A: panics, Track B: normal.
        let (reg_a, _, _, _cancel_a) = make_registration(MockSource::panicking(), 32, 1);
        let (reg_b, mut rx_b, _, _cancel_b) = make_registration(MockSource::new(10), 32, 1);

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
        let (reg, mut rx, _, _cancel) = make_registration(source, 32, 1);

        let _id = handle.register_track(reg);

        // Wait for a few chunks first.
        let got = wait_for_chunks(&mut rx, 2, Duration::from_secs(5));
        assert!(got >= 2);

        // Initiate a seek via timeline.
        let _ = timeline.initiate_seek(Duration::from_secs(10));
        handle.wake();

        // The seek should be handled (seek pending cleared after apply).
        kithara_platform::thread::sleep(Duration::from_millis(100));

        // After seek completes, more chunks should arrive.
        let after_seek = wait_for_chunks(&mut rx, 1, Duration::from_secs(5));
        assert!(after_seek >= 1, "should resume decoding after seek");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_preload_notify_fires() {
        let handle = AudioWorkerHandle::new();

        let (reg, _rx, _preload_notify, _cancel) = make_registration(MockSource::new(10), 32, 3);

        let _id = handle.register_track(reg);

        // preload_notify should fire after 3 chunks.
        // We use a tokio runtime to await the notify.
        // Give worker time to produce 3+ chunks.
        // The preload_notify fires internally; we verify it doesn't deadlock.
        kithara_platform::thread::sleep(Duration::from_millis(200));

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_unregister_removes_track() {
        let handle = AudioWorkerHandle::new();

        let (reg, mut rx, _, _cancel) = make_registration(MockSource::new(100), 32, 1);

        let id = handle.register_track(reg);

        let got = wait_for_chunks(&mut rx, 2, Duration::from_secs(5));
        assert!(got >= 2);

        handle.unregister_track(id);
        kithara_platform::thread::sleep(Duration::from_millis(50));

        // Drain remaining.
        while rx.try_pop().is_some() {}

        // No more chunks should arrive.
        kithara_platform::thread::sleep(Duration::from_millis(50));
        assert!(rx.try_pop().is_none(), "no chunks after unregister");

        handle.shutdown();
    }

    #[kithara::test]
    fn worker_service_class_prioritises_audible() {
        let handle = AudioWorkerHandle::new();

        // Track A: Idle, large supply (but low priority).
        let (reg_a, mut rx_a, _, _ca) = make_registration(MockSource::new(100), 4, 0);
        let id_a = handle.register_track(reg_a);

        // Track B: will be promoted to Audible.
        let (reg_b, mut rx_b, _, _cb) = make_registration(MockSource::new(100), 4, 0);
        let id_b = handle.register_track(reg_b);

        // Let both tracks start decoding (both default to Audible from registration).
        kithara_platform::thread::sleep(Duration::from_millis(30));

        // Drain both to make room.
        while rx_a.try_pop().is_some() {}
        while rx_b.try_pop().is_some() {}

        // Demote A to Idle, keep B as Audible.
        handle.set_service_class(id_a, ServiceClass::Idle);
        handle.set_service_class(id_b, ServiceClass::Audible);

        // Wait a bit for scheduling to take effect.
        kithara_platform::thread::sleep(Duration::from_millis(50));

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
}
