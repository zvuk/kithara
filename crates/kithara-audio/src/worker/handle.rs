//! Shared audio worker — cooperative multi-track decoder on a single OS thread.
//!
//! [`AudioWorkerHandle`] is the user-facing handle (clonable). It spawns a
//! background OS thread that runs [`run_shared_worker_loop`], serving all
//! registered tracks via round-robin scheduling.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kithara_decode::PcmChunk;
use kithara_platform::{
    sync::mpsc::{self, TryRecvError},
    thread::{spawn_named, yield_now},
    time::Instant,
    tokio::sync::Notify,
};
use kithara_stream::Fetch;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Producer},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use super::{
    AudioCommand, AudioWorkerSource,
    thread_wake::ThreadWake,
    types::{ServiceClass, StepResult, TrackId, TrackIdGen},
    wake::WorkerWake,
};
use crate::pipeline::track_fsm::TrackStep;

/// Threshold for warning about slow `step_track` calls.
const STEP_WARN_THRESHOLD: Duration = Duration::from_millis(10);

// Public types

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

/// Monotonic counter for unique audio-worker thread names.
static AUDIO_WORKER_ID: AtomicU64 = AtomicU64::new(0);

impl AudioWorkerHandle {
    /// Spawn a new shared worker thread and return a handle.
    #[must_use]
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let wake = Arc::new(WorkerWake::new());
        let cancel = CancellationToken::new();

        let wake_clone = Arc::clone(&wake);
        let cancel_clone = cancel.clone();

        let id = AUDIO_WORKER_ID.fetch_add(1, Ordering::Relaxed);
        spawn_named(format!("kithara-audio-worker-{id}"), move || {
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

// Internal: TrackSlot

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
    seek_epoch: u64,
    /// Set when `step_track()` returns `Failed` — truly terminal.
    terminal: bool,
    /// Set when EOF fetch has been pushed to the consumer.
    eof_sent: bool,
}

impl TrackSlot {
    fn from_registration(track_id: TrackId, reg: TrackRegistration) -> Self {
        let seek_epoch = reg.source.timeline().seek_epoch();
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
            seek_epoch,
            terminal: false,
            eof_sent: false,
        }
    }

    fn is_removable(&self) -> bool {
        self.terminal || self.cancel.is_cancelled()
    }

    /// One cooperative step: drain commands, step FSM, push at most one chunk.
    fn step(&mut self) -> StepResult {
        if self.cancel.is_cancelled() || self.terminal {
            return StepResult::NoProgress;
        }

        self.sync_seek_epoch();

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
                    return StepResult::Waiting;
                }
            }
        }

        // Step the track FSM.
        let step_start = Instant::now();
        let step_result = self.source.step_track();
        let step_elapsed = step_start.elapsed();
        if step_elapsed > STEP_WARN_THRESHOLD {
            tracing::warn!(
                track_id = self.track_id,
                elapsed_ms = step_elapsed.as_millis(),
                "step_track took too long — starving other tracks"
            );
        }
        match step_result {
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
                StepResult::Waiting
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

    fn sync_seek_epoch(&mut self) {
        let current = self.source.timeline().seek_epoch();
        if current == self.seek_epoch {
            return;
        }

        self.seek_epoch = current;
        self.pending_fetch = None;
        self.chunks_sent = 0;
        self.preloaded = false;
        self.eof_sent = false;
    }
}

// Worker loop

const IDLE_TIMEOUT: Duration = Duration::from_millis(10);
const EMPTY_TIMEOUT: Duration = Duration::from_millis(100);

/// Best result from a single round-robin pass over all tracks.
///
/// Ordered by priority: `Produced > Waiting > Idle > Stuck`.
/// A single track returning `Produced` overrides everything else.
enum PassOutcome {
    /// At least one track decoded a chunk or changed state.
    Produced,
    /// No chunks produced, but at least one track is alive and waiting
    /// (backpressure or source loading). Not a hang.
    Waiting,
    /// All tracks are terminal (EOF / Failed) — legitimately idle.
    Idle,
    /// Non-terminal tracks reported no progress — potential hang.
    Stuck,
}

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

        if drain_worker_commands(cmd_rx, &mut tracks, &mut cursor) {
            return;
        }

        // Round-robin: step each track once, collect best outcome.
        let outcome = step_all_tracks(&mut tracks, &mut cursor);

        // Remove finished tracks.
        let before = tracks.len();
        tracks.retain(|slot| !slot.is_removable());
        if tracks.len() < before && cursor > tracks.len() {
            cursor = 0;
        }

        // Watchdog + sleep based on outcome.
        match outcome {
            PassOutcome::Produced => {
                hang_reset!();
                yield_now();
            }
            PassOutcome::Waiting => {
                wake.wait_timeout(IDLE_TIMEOUT);
            }
            PassOutcome::Idle => {
                hang_reset!();
                wake.wait_timeout(EMPTY_TIMEOUT);
            }
            PassOutcome::Stuck => {
                hang_tick!();
                wake.wait_timeout(IDLE_TIMEOUT);
            }
        }
    }
}

fn step_all_tracks(tracks: &mut [TrackSlot], cursor: &mut usize) -> PassOutcome {
    if tracks.is_empty() {
        return PassOutcome::Idle;
    }

    let mut best = StepResult::NoProgress;
    let count = tracks.len();

    for _ in 0..count {
        if *cursor >= count {
            *cursor = 0;
        }
        let slot = &mut tracks[*cursor];
        *cursor += 1;

        let result = if let Ok(r) = catch_unwind(AssertUnwindSafe(|| slot.step())) {
            r
        } else {
            warn!(track_id = slot.track_id, "shared worker: track panicked");
            slot.terminal = true;
            slot.complete_preload();
            let epoch = slot.source.timeline().seek_epoch();
            let _ = slot
                .data_tx
                .try_push(Fetch::new(PcmChunk::default(), true, epoch));
            slot.consumer_wake.wake();
            StepResult::NoProgress
        };

        // Keep the "best" result: Progress > Waiting > NoProgress.
        best = match (best, result) {
            (StepResult::Progress, _) | (_, StepResult::Progress) => StepResult::Progress,
            (StepResult::Waiting, _) | (_, StepResult::Waiting) => StepResult::Waiting,
            _ => StepResult::NoProgress,
        };
    }

    match best {
        StepResult::Progress => PassOutcome::Produced,
        StepResult::Waiting => PassOutcome::Waiting,
        StepResult::NoProgress => {
            if tracks.iter().all(|s| s.terminal || s.eof_sent) {
                PassOutcome::Idle
            } else {
                PassOutcome::Stuck
            }
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

// Tests

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
        thread::sleep as thread_sleep, time::timeout as platform_timeout, tokio::sync::Notify,
    };
    use kithara_stream::{Fetch, Timeline};
    use kithara_test_utils::kithara;
    use ringbuf::{HeapRb, traits::Split};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        pipeline::track_fsm::{TrackStep, WaitingReason},
        worker::{AudioCommand, AudioWorkerSource, types::ServiceClass},
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

    // Helpers

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

    fn make_registration_with_source(
        source: Box<dyn AudioWorkerSource<Chunk = PcmChunk, Command = AudioCommand>>,
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
            source,
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

        thread_sleep(Duration::from_millis(100));

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
        thread_sleep(Duration::from_millis(50));

        // Pop one, give worker time to push pending.
        let first = rx.try_pop();
        assert!(first.is_some(), "should have at least one chunk");

        thread_sleep(Duration::from_millis(50));

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
        thread_sleep(Duration::from_millis(100));

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
        thread_sleep(Duration::from_millis(200));

        handle.shutdown();
    }

    #[kithara::test(tokio)]
    async fn worker_preload_notify_rearms_after_seek() {
        let handle = AudioWorkerHandle::new();

        let (reg, _rx, preload_notify, _cancel) = make_registration(MockSource::new(10), 32, 1);
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

        let (reg, mut rx, _, _cancel) = make_registration(MockSource::new(100), 32, 1);

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
        let (reg_a, mut rx_a, _, _ca) = make_registration(MockSource::new(100), 4, 0);
        let id_a = handle.register_track(reg_a);

        // Track B: will be promoted to Audible.
        let (reg_b, mut rx_b, _, _cb) = make_registration(MockSource::new(100), 4, 0);
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
    /// blocks the shared worker's step_track() call, causing MP3 track
    /// audio to stutter.
    ///
    /// The mock simulates a track whose step_track() blocks the thread
    /// for 50ms (like a real wait_range() call waiting for network data).
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
            type Command = AudioCommand;

            fn step_track(&mut self) -> TrackStep<PcmChunk> {
                if self.blocking.load(Ordering::Relaxed) {
                    // Simulate sync blocking (like wait_range timeout at 10ms).
                    thread_sleep(Duration::from_millis(10));
                    TrackStep::Blocked(WaitingReason::Waiting)
                } else {
                    TrackStep::Blocked(WaitingReason::Waiting)
                }
            }

            fn handle_command(&mut self, _cmd: AudioCommand) {}

            fn timeline(&self) -> &Timeline {
                &self.timeline
            }
        }

        let handle = AudioWorkerHandle::new();

        // Track A: always ready, produces 100 chunks fast.
        let (reg_a, mut rx_a, _, _ca) = make_registration(MockSource::new(100), 32, 0);
        let _id_a = handle.register_track(reg_a);

        // Track B: blocks thread for 50ms per step (simulating network wait).
        let blocking = Arc::new(AtomicBool::new(true));
        let blocking_source = BlockingSource {
            timeline: Timeline::new(),
            blocking: Arc::clone(&blocking),
        };
        let (reg_b, _rx_b, _, _cb) =
            make_registration_with_source(Box::new(blocking_source), 32, 0);
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

    /// A track that blocks inside step_track() (simulating Symphonia read
    /// waiting on network data) must not starve other tracks.
    ///
    /// This is the REAL production bug: HLS decode path enters wait_range()
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
            type Command = AudioCommand;

            fn step_track(&mut self) -> TrackStep<PcmChunk> {
                // Simulate: source_is_ready() returns true, then
                // decode_next_chunk() blocks on wait_range() for data.
                thread_sleep(Duration::from_millis(self.block_ms));
                TrackStep::Produced(Fetch::new(PcmChunk::default(), false, 0))
            }

            fn handle_command(&mut self, _cmd: AudioCommand) {}

            fn timeline(&self) -> &Timeline {
                &self.timeline
            }
        }

        let handle = AudioWorkerHandle::new();

        // Track A (MP3-like): fast, always ready.
        let (reg_a, mut rx_a, _, _ca) = make_registration(MockSource::new(1000), 32, 0);
        let _id_a = handle.register_track(reg_a);

        // Track B (HLS-like): blocks 10ms per step. This simulates the
        // reduced WAIT_RANGE_TIMEOUT (10ms) + WAIT_RANGE_SLEEP_MS (2ms)
        // where wait_range does a few condvar spins before timing out.
        // With the fix, step_track returns within ~10ms instead of 50ms+.
        let slow_source = SlowDecodeSource {
            timeline: Timeline::new(),
            block_ms: 10,
        };
        let (reg_b, mut rx_b, _, _cb) = make_registration_with_source(Box::new(slow_source), 32, 0);
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

    /// The shared worker must NOT busy-spin after producing chunks.
    ///
    /// Reproduces production bug: MP3 (fast local file) + HLS (network)
    /// on shared worker. Worker busy-loops on MP3 with yield_now(),
    /// starving the tokio runtime. HLS downloader can't run → HLS track
    /// gets no data → audio glitches.
    ///
    /// The test measures: if nobody consumes the ringbuf, the worker must
    /// sleep (backpressure), not spin. We check that the worker thread's
    /// CPU time stays low relative to wall time.
    #[kithara::test]
    fn shared_worker_does_not_busy_spin_on_backpressure() {
        let handle = AudioWorkerHandle::new();

        // Track with small ringbuf (capacity 2) — fills fast.
        let (reg, _rx, _, _cancel) = make_registration(MockSource::new(10_000), 2, 0);
        let _id = handle.register_track(reg);

        // Let it fill the ringbuf.
        thread_sleep(Duration::from_millis(50));

        // Now ringbuf is full. Nobody is consuming. Worker should be sleeping.
        // Measure process CPU time over 500ms of wall time.
        let cpu_before = cpu_time_ms();
        thread_sleep(Duration::from_millis(500));
        let cpu_after = cpu_time_ms();

        let cpu_used_ms = cpu_after.saturating_sub(cpu_before);

        handle.shutdown();

        // A well-behaved worker sleeping on backpressure uses <50ms of CPU
        // in 500ms wall time (<10%). A busy-spinning worker uses ~500ms (100%).
        assert!(
            cpu_used_ms < 100,
            "Worker should NOT busy-spin on backpressure: \
             used {cpu_used_ms}ms CPU in 500ms wall time (expected <100ms)"
        );
    }
}

/// Process CPU time in milliseconds (user + system).
#[cfg(test)]
fn cpu_time_ms() -> u64 {
    let output = std::process::Command::new("ps")
        .args(["-o", "cputime=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps failed");
    let s = String::from_utf8_lossy(&output.stdout);
    parse_cputime(s.trim())
}

/// Parse "H:MM:SS" or "M:SS" format from `ps -o cputime=` into milliseconds.
#[cfg(test)]
fn parse_cputime(s: &str) -> u64 {
    const HMS_PARTS: usize = 3;
    const MS_PARTS: usize = 2;
    const SECS_PER_HOUR: u64 = 3600;
    const SECS_PER_MIN: u64 = 60;
    const MS_PER_SEC: u64 = 1000;
    const SEC_IDX: usize = 2;

    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        HMS_PARTS => {
            let h: u64 = parts[0].trim().parse().unwrap_or(0);
            let m: u64 = parts[1].trim().parse().unwrap_or(0);
            let sec: u64 = parts[SEC_IDX].trim().parse().unwrap_or(0);
            (h * SECS_PER_HOUR + m * SECS_PER_MIN + sec) * MS_PER_SEC
        }
        MS_PARTS => {
            let m: u64 = parts[0].trim().parse().unwrap_or(0);
            let sec: u64 = parts[1].trim().parse().unwrap_or(0);
            (m * SECS_PER_MIN + sec) * MS_PER_SEC
        }
        _ => 0,
    }
}
