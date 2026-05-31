use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::Duration,
};

use kithara_platform::{
    sync::mpsc::{self, TryRecvError},
    thread::{spawn_named, yield_now},
    time::Instant,
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::runtime::{
    Node, PassOutcome, SchedulerEvent, SchedulerObserver, SchedulerWake, ServiceClass, TickResult,
};

/// Unique identifier for a slot in the scheduler.
pub(crate) type SlotId = u64;

/// Command sent from `SchedulerHandle` to the scheduler thread.
pub(crate) enum SchedulerCmd<N> {
    /// Register a new node.
    Register(SlotId, N),
    /// Remove a node by ID.
    Unregister(SlotId),
    /// Graceful shutdown — exit the scheduler loop.
    Shutdown,
}

/// A slot holding a node and its metadata.
pub(crate) struct Slot<N> {
    pub(crate) node: N,
    pub(crate) service_class: ServiceClass,
    pub(crate) id: SlotId,
    pub(crate) is_terminal: bool,
}

impl<N: Node> Slot<N> {
    fn is_removable(&self) -> bool {
        self.is_terminal
    }
}

/// Clonable handle to a scheduler.
pub(crate) struct SchedulerHandle<N> {
    inner: Arc<SchedulerInner<N>>,
}

impl<N> Clone for SchedulerHandle<N> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct SchedulerInner<N> {
    wake: Arc<SchedulerWake>,
    cancel: CancellationToken,
    cmd_tx: mpsc::Sender<SchedulerCmd<N>>,
}

impl<N> SchedulerInner<N> {
    fn shutdown(&self) {
        let _ = self.cmd_tx.send_sync(SchedulerCmd::Shutdown);
        self.cancel.cancel();
        self.wake.wake();
    }
}

impl<N> Drop for SchedulerInner<N> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl<N: Node> SchedulerHandle<N> {
    /// Register a node.
    pub(crate) fn register(&self, id: SlotId, node: N) {
        if self
            .inner
            .cmd_tx
            .send_sync(SchedulerCmd::Register(id, node))
            .is_err()
        {
            warn!(slot_id = id, "register: scheduler channel closed");
        }
        self.inner.wake.wake();
    }

    /// Request graceful shutdown and cancel the scheduler.
    pub(crate) fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Remove a node by ID.
    pub(crate) fn unregister(&self, id: SlotId) {
        if self
            .inner
            .cmd_tx
            .send_sync(SchedulerCmd::Unregister(id))
            .is_err()
        {
            warn!(slot_id = id, "unregister: scheduler channel closed");
        }
        self.inner.wake.wake();
    }

    /// Wake the scheduler.
    pub(crate) fn wake(&self) {
        self.inner.wake.wake();
    }
}

/// The core scheduler.
pub(crate) struct Scheduler<N, O> {
    _phantom: std::marker::PhantomData<(N, O)>,
}

impl<N: Node, O: SchedulerObserver> Scheduler<N, O> {
    /// Park budget used after `PassOutcome::Idle` — every slot is
    /// terminal (or no slots at all), no work expected soon, so we
    /// park longer to keep CPU idle.
    const IDLE_TIMEOUT: Duration = Duration::from_millis(100);
    /// Threshold for warning about slow `tick` calls.
    const SLOW_TICK_THRESHOLD: Duration = Duration::from_millis(10);
    /// Park budget used after `PassOutcome::Waiting` /
    /// `PassOutcome::Backpressured` — at least one slot is alive and
    /// likely to make progress shortly (source becomes ready,
    /// consumer drains the PCM ring), so re-check more aggressively.
    const WAITING_TIMEOUT: Duration = Duration::from_millis(10);

    /// Spawn a new scheduler thread and return a handle.
    ///
    /// `cancel` is the externally-owned token that drives the run loop's
    /// shutdown. Callers (e.g. [`AudioWorkerHandle`](super::super::worker::AudioWorkerHandle))
    /// derive it as a child of the player master so worker shutdown
    /// participates in the unified cancel hierarchy.
    #[must_use]
    pub(crate) fn start(
        name: String,
        observer: O,
        cancel: CancellationToken,
    ) -> SchedulerHandle<N> {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let wake = Arc::new(SchedulerWake::default());

        let wake_clone = Arc::clone(&wake);
        let cancel_clone = cancel.clone();

        spawn_named(name, move || {
            run_loop(&cmd_rx, &wake_clone, &cancel_clone, observer);
        });

        SchedulerHandle {
            inner: Arc::new(SchedulerInner {
                wake,
                cancel,
                cmd_tx,
            }),
        }
    }
}

#[kithara::rtsan_forbid_blocking]
fn run_loop<N: Node, O: SchedulerObserver>(
    cmd_rx: &mpsc::Receiver<SchedulerCmd<N>>,
    wake: &SchedulerWake,
    cancel: &CancellationToken,
    mut observer: O,
) {
    trace!("scheduler started");
    let mut slots: Vec<Slot<N>> = Vec::new();
    let mut slots_order: Vec<usize> = Vec::new();
    let mut needs_reorder = false;

    loop {
        observer.on_event(SchedulerEvent::PassStart);

        if cancel_and_drain(cancel, cmd_rx, &mut slots, &mut needs_reorder) {
            dispose_on_exit(slots, slots_order);
            return;
        }

        refresh_service_classes(&mut slots, &mut needs_reorder);

        if needs_reorder {
            recompute_slots_order(&slots, &mut slots_order);
            needs_reorder = false;
        }

        let outcome = step_all_slots(&mut slots, &slots_order, &mut observer);

        needs_reorder |= reap_terminal_slots(&mut slots);

        report_outcome(&mut observer, outcome);
        observer.on_event(SchedulerEvent::PassEnd);
        park_after_outcome::<N, O>(wake, outcome);
    }
}

#[kithara::rtsan_allow_blocking]
fn cancel_and_drain<N: Node>(
    cancel: &CancellationToken,
    cmd_rx: &mpsc::Receiver<SchedulerCmd<N>>,
    slots: &mut Vec<Slot<N>>,
    needs_reorder: &mut bool,
) -> bool {
    if cancel.is_cancelled() {
        trace!("scheduler cancelled");
        for slot in slots.iter_mut() {
            slot.node.on_cancel();
        }
        return true;
    }
    drain_commands(cmd_rx, slots, needs_reorder)
}

fn report_outcome<O: SchedulerObserver>(observer: &mut O, outcome: PassOutcome) {
    match outcome {
        PassOutcome::Produced => observer.on_event(SchedulerEvent::Progress),
        PassOutcome::Waiting => observer.on_event(SchedulerEvent::Waiting),
        PassOutcome::Backpressured => observer.on_event(SchedulerEvent::Backpressured),
        PassOutcome::Idle => observer.on_event(SchedulerEvent::Idle),
    }
}

#[kithara::rtsan_allow_blocking]
fn park_after_outcome<N: Node, O: SchedulerObserver>(wake: &SchedulerWake, outcome: PassOutcome) {
    match outcome {
        PassOutcome::Produced => yield_now(),
        PassOutcome::Waiting | PassOutcome::Backpressured => {
            wake.wait_timeout(Scheduler::<N, O>::WAITING_TIMEOUT);
        }
        PassOutcome::Idle => {
            wake.wait_timeout(Scheduler::<N, O>::IDLE_TIMEOUT);
        }
    }
}

#[kithara::rtsan_allow_blocking]
fn recompute_slots_order<N: Node>(slots: &[Slot<N>], slots_order: &mut Vec<usize>) {
    slots_order.clear();
    slots_order.reserve(slots.len());
    slots_order.extend(0..slots.len());
    slots_order.sort_by(|&a, &b| {
        let class_a = slots[a].service_class;
        let class_b = slots[b].service_class;
        class_b.cmp(&class_a)
    });
}

/// Remove and drop terminal slots, returning `true` when any were removed so
/// the caller re-sorts. Dropping a finished node frees its decoder buffers —
/// bounded teardown, not real-time production — so it runs in a permitted
/// scope, off the `forbid_blocking` checked path.
#[kithara::rtsan_allow_blocking]
fn reap_terminal_slots<N: Node>(slots: &mut Vec<Slot<N>>) -> bool {
    let before = slots.len();
    slots.retain(|slot| !slot.is_removable());
    slots.len() < before
}

/// Drop the scheduler working sets when the loop exits on cancel/shutdown.
/// Frees node buffers and the slot vectors in a permitted scope — same
/// rationale as [`reap_terminal_slots`]. The explicit `drop`s keep the frees
/// inside the permit guard, ahead of the parameter scope.
#[kithara::rtsan_allow_blocking]
fn dispose_on_exit<N>(slots: Vec<Slot<N>>, slots_order: Vec<usize>) {
    drop(slots);
    drop(slots_order);
}

fn step_all_slots<N: Node, O: SchedulerObserver>(
    slots: &mut [Slot<N>],
    slots_order: &[usize],
    observer: &mut O,
) -> PassOutcome {
    if slots.is_empty() {
        return PassOutcome::Idle;
    }

    let mut best = TickResult::Done;

    for &idx in slots_order {
        if idx >= slots.len() {
            continue;
        }
        let slot = &mut slots[idx];

        let start = Instant::now();
        let result = if let Ok(r) = catch_unwind(AssertUnwindSafe(|| slot.node.tick())) {
            r
        } else {
            warn!(slot_id = slot.id, "scheduler: node panicked");
            slot.is_terminal = true;
            slot.node.on_cancel();
            TickResult::Done
        };
        let elapsed = start.elapsed();

        if elapsed > Scheduler::<N, O>::SLOW_TICK_THRESHOLD {
            observer.on_event(SchedulerEvent::SlowTick {
                elapsed,
                slot: slot.id,
            });
        }

        if result == TickResult::Done {
            slot.is_terminal = true;
        }

        best = match (best, result) {
            (TickResult::Progress, _) | (_, TickResult::Progress) => TickResult::Progress,
            (TickResult::Waiting, _) | (_, TickResult::Waiting) => TickResult::Waiting,
            (TickResult::Backpressured, _) | (_, TickResult::Backpressured) => {
                TickResult::Backpressured
            }
            _ => TickResult::Done,
        };
    }

    match best {
        TickResult::Progress => PassOutcome::Produced,
        TickResult::Waiting => PassOutcome::Waiting,
        TickResult::Backpressured => PassOutcome::Backpressured,
        TickResult::Done => PassOutcome::Idle,
    }
}

/// Outcome of processing a single scheduler command.
enum DrainStep {
    /// Command applied; keep draining the queue.
    Continue,
    /// Queue is empty for now; bail out and resume the audio loop.
    Empty,
    /// Scheduler shutdown was requested (or all handles dropped); exit thread.
    Shutdown,
}

fn drain_commands<N: Node>(
    cmd_rx: &mpsc::Receiver<SchedulerCmd<N>>,
    slots: &mut Vec<Slot<N>>,
    needs_reorder: &mut bool,
) -> bool {
    loop {
        match handle_drain_step(cmd_rx, slots, needs_reorder) {
            DrainStep::Continue => {}
            DrainStep::Empty => return false,
            DrainStep::Shutdown => return true,
        }
    }
}

fn handle_drain_step<N: Node>(
    cmd_rx: &mpsc::Receiver<SchedulerCmd<N>>,
    slots: &mut Vec<Slot<N>>,
    needs_reorder: &mut bool,
) -> DrainStep {
    match cmd_rx.try_recv() {
        Ok(SchedulerCmd::Register(id, node)) => {
            register_slot(slots, needs_reorder, id, node);
            DrainStep::Continue
        }
        Ok(SchedulerCmd::Unregister(id)) => {
            unregister_slot(slots, needs_reorder, id);
            DrainStep::Continue
        }
        Ok(SchedulerCmd::Shutdown) => {
            trace!("scheduler shutdown");
            cancel_all(slots);
            DrainStep::Shutdown
        }
        Err(TryRecvError::Disconnected) => {
            trace!("scheduler: all handles dropped");
            cancel_all(slots);
            DrainStep::Shutdown
        }
        Err(_) => DrainStep::Empty,
    }
}

fn register_slot<N: Node>(slots: &mut Vec<Slot<N>>, needs_reorder: &mut bool, id: SlotId, node: N) {
    debug!(slot_id = id, "scheduler: registering node");
    let service_class = node.service_class();
    slots.push(Slot {
        id,
        node,
        service_class,
        is_terminal: false,
    });
    *needs_reorder = true;
}

fn unregister_slot<N: Node>(slots: &mut Vec<Slot<N>>, needs_reorder: &mut bool, id: SlotId) {
    debug!(slot_id = id, "scheduler: unregistering node");
    if let Some(slot) = slots.iter_mut().find(|s| s.id == id) {
        slot.node.on_cancel();
    }
    let before = slots.len();
    slots.retain(|s| s.id != id);
    if slots.len() < before {
        *needs_reorder = true;
    }
}

/// Re-read each node's (atomic) service class and flag a reorder when one
/// changed. The real-time consumer updates the shared atomic wait-free and
/// wakes the worker; the scheduler picks the change up here on its next
/// pass, so priority changes need no command-channel round-trip.
fn refresh_service_classes<N: Node>(slots: &mut [Slot<N>], needs_reorder: &mut bool) {
    for slot in slots.iter_mut() {
        let current = slot.node.service_class();
        if slot.service_class != current {
            slot.service_class = current;
            *needs_reorder = true;
        }
    }
}

fn cancel_all<N: Node>(slots: &mut [Slot<N>]) {
    for slot in slots.iter_mut() {
        slot.node.on_cancel();
    }
}

/// Process CPU time in milliseconds (user + system).
#[cfg(test)]
pub(crate) fn cpu_time_ms() -> u64 {
    let output = std::process::Command::new("ps")
        .args(["-o", "cputime=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps failed");
    let s = String::from_utf8_lossy(&output.stdout);
    parse_cputime(s.trim())
}

/// Parse "H:MM:SS" or "M:SS" format from `ps -o cputime=` into milliseconds.
#[cfg(test)]
pub(crate) fn parse_cputime(s: &str) -> u64 {
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

#[cfg(test)]
mod tests {
    use kithara_platform::thread::sleep;
    use kithara_test_utils::kithara;

    use super::*;

    struct TestObserver;

    impl SchedulerObserver for TestObserver {
        fn on_event(&mut self, _event: SchedulerEvent) {}
    }

    struct DummyNode {
        panic_at: Option<usize>,
        max_ticks: usize,
        ticks: usize,
    }

    impl Node for DummyNode {
        fn tick(&mut self) -> TickResult {
            if let Some(p) = self.panic_at
                && self.ticks == p
            {
                panic!("dummy panic");
            }
            if self.ticks >= self.max_ticks {
                TickResult::Done
            } else {
                self.ticks += 1;
                TickResult::Progress
            }
        }
    }

    struct ServiceClassNode {
        service_class: ServiceClass,
    }

    impl Node for ServiceClassNode {
        fn service_class(&self) -> ServiceClass {
            self.service_class
        }

        fn tick(&mut self) -> TickResult {
            TickResult::Done
        }
    }

    #[kithara::test]
    fn scheduler_creates_and_drops_cleanly() {
        let handle = Scheduler::<DummyNode, TestObserver>::start(
            "test-worker".into(),
            TestObserver,
            CancellationToken::new(),
        );
        sleep(Duration::from_millis(10));
        handle.shutdown();
        sleep(Duration::from_millis(50));
    }

    #[kithara::test]
    fn scheduler_panic_isolation() {
        let handle = Scheduler::<DummyNode, TestObserver>::start(
            "test-worker".into(),
            TestObserver,
            CancellationToken::new(),
        );

        handle.register(
            1,
            DummyNode {
                ticks: 0,
                max_ticks: 10,
                panic_at: Some(2),
            },
        );

        handle.register(
            2,
            DummyNode {
                ticks: 0,
                max_ticks: 10,
                panic_at: None,
            },
        );

        sleep(Duration::from_millis(100));
        handle.shutdown();
    }

    struct BackpressureNode;

    impl Node for BackpressureNode {
        fn tick(&mut self) -> TickResult {
            TickResult::Waiting
        }
    }

    #[kithara::test]
    fn scheduler_does_not_busy_spin_on_backpressure() {
        let handle = Scheduler::<BackpressureNode, TestObserver>::start(
            "test-worker".into(),
            TestObserver,
            CancellationToken::new(),
        );

        handle.register(1, BackpressureNode);

        sleep(Duration::from_millis(50));

        let cpu_before = cpu_time_ms();
        sleep(Duration::from_millis(500));
        let cpu_after = cpu_time_ms();

        let cpu_used_ms = cpu_after.saturating_sub(cpu_before);

        handle.shutdown();

        assert!(
            cpu_used_ms < 100,
            "Worker should NOT busy-spin on backpressure: \
             used {cpu_used_ms}ms CPU in 500ms wall time (expected <100ms)"
        );
    }

    #[kithara::test]
    fn scheduler_orders_service_classes_descending() {
        let slots = vec![
            Slot {
                id: 1,
                node: ServiceClassNode {
                    service_class: ServiceClass::Idle,
                },
                service_class: ServiceClass::Idle,
                is_terminal: false,
            },
            Slot {
                id: 2,
                node: ServiceClassNode {
                    service_class: ServiceClass::Audible,
                },
                service_class: ServiceClass::Audible,
                is_terminal: false,
            },
            Slot {
                id: 3,
                node: ServiceClassNode {
                    service_class: ServiceClass::Warm,
                },
                service_class: ServiceClass::Warm,
                is_terminal: false,
            },
        ];

        let mut slots_order: Vec<usize> = Vec::new();
        recompute_slots_order(&slots, &mut slots_order);

        assert_eq!(slots_order, vec![1, 2, 0]);
    }

    #[kithara::test]
    fn recompute_slots_order_keeps_capacity_stable() {
        let slots: Vec<Slot<ServiceClassNode>> = (0..8)
            .map(|id| Slot {
                id,
                node: ServiceClassNode {
                    service_class: ServiceClass::Warm,
                },
                service_class: ServiceClass::Warm,
                is_terminal: false,
            })
            .collect();

        let mut slots_order: Vec<usize> = Vec::new();
        recompute_slots_order(&slots, &mut slots_order);

        let cap = slots_order.capacity();
        let ptr = slots_order.as_ptr() as usize;
        assert_eq!(slots_order.len(), slots.len());

        for _ in 0..100 {
            recompute_slots_order(&slots, &mut slots_order);
        }

        assert_eq!(
            slots_order.capacity(),
            cap,
            "steady recompute must not grow the slots_order backing"
        );
        assert_eq!(
            slots_order.as_ptr() as usize,
            ptr,
            "clear() retains capacity so the backing is never reallocated"
        );
    }
}
