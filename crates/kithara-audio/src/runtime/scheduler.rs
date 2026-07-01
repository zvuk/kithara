use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use kithara_platform::{
    CancelToken,
    sync::mpsc::{self, TryRecvError},
    thread::{spawn_named, yield_now},
    time::{Duration, Instant},
};
use kithara_test_utils::kithara;
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
    cancel: CancelToken,
    cmd_tx: mpsc::Sender<SchedulerCmd<N>>,
}

impl<N> SchedulerInner<N> {
    fn shutdown(&self) {
        let _ = self.cmd_tx.send(SchedulerCmd::Shutdown);
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
            .send(SchedulerCmd::Register(id, node))
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
            .send(SchedulerCmd::Unregister(id))
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
    /// `PassOutcome::UpstreamPending` / `PassOutcome::Backpressured` —
    /// at least one slot is alive and likely to make progress shortly
    /// (source becomes ready, consumer drains the PCM ring), so re-check
    /// more aggressively.
    const WAITING_TIMEOUT: Duration = Duration::from_millis(10);

    /// Spawn a new scheduler thread and return a handle.
    ///
    /// `cancel` is the externally-owned [`CancelToken`] that drives the run
    /// loop's shutdown. Callers (e.g.
    /// [`AudioWorkerHandle`](super::super::worker::AudioWorkerHandle)) derive
    /// it as a `child()` of the player master so worker shutdown
    /// participates in the unified cancel hierarchy and the lock-free
    /// `is_cancelled()` read on the produce-core observes a master cancel.
    #[must_use]
    pub(crate) fn start(name: String, observer: O, cancel: CancelToken) -> SchedulerHandle<N> {
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

/// Consecutive `Produced` passes between cooperative `yield_now()` calls.
///
/// The produce core never parks while it is making progress; a bare
/// per-pass `yield_now()` would still hand the scheduler off on every chunk.
/// Yielding only every Nth straight `Produced` pass keeps the worker hot
/// under sustained decode while still ceding the CPU often enough to stay
/// fair to other threads. The streak resets on any non-`Produced` outcome,
/// which already parks via `wait_timeout`.
const FAIRNESS_YIELD_EVERY: u32 = 16;

/// Unchecked scheduler shell.
///
/// Owns all intrinsically-blocking bookkeeping — cancel/command drain, slot
/// reorder, the `slots`/`slots_order` Vec lifecycle (grow on register, free
/// on `retain`), deferred buffer recycle, and the idle park. Only the
/// produce core ([`produce_pass`]) is `#[kithara::rtsan_forbid_blocking]`,
/// so the Vec `malloc`/`free` here never lands on the checked path.
#[kithara::flash(true)]
fn run_loop<N: Node, O: SchedulerObserver>(
    cmd_rx: &mpsc::Receiver<SchedulerCmd<N>>,
    wake: &SchedulerWake,
    cancel: &CancelToken,
    mut observer: O,
) {
    trace!("scheduler started");
    let mut slots: Vec<Slot<N>> = Vec::new();
    let mut slots_order: Vec<usize> = Vec::new();
    let mut needs_reorder = false;
    let mut produced_streak: u32 = 0;

    loop {
        observer.on_event(SchedulerEvent::PassStart);

        if cancel_and_drain(cancel, cmd_rx, &mut slots, &mut needs_reorder) {
            return;
        }

        refresh_service_classes(&mut slots, &mut needs_reorder);

        if needs_reorder {
            recompute_slots_order(&slots, &mut slots_order);
            needs_reorder = false;
        }

        recycle_all(&mut slots, &slots_order);

        let outcome = produce_pass(&mut slots, &slots_order, &mut observer);

        let before = slots.len();
        slots.retain(|slot| !slot.is_removable());
        needs_reorder |= slots.len() < before;

        report_outcome(&mut observer, outcome);
        observer.on_event(SchedulerEvent::PassEnd);
        park_after_outcome::<N, O>(wake, outcome, &mut produced_streak);
    }
}

/// Reclaim deferred bookkeeping (spent-buffer free/recycle) for every slot
/// before the checked produce core runs. Lives in the unchecked shell so a
/// pooled-buffer `free` on a full pool stays off the forbid-blocking path.
fn recycle_all<N: Node>(slots: &mut [Slot<N>], slots_order: &[usize]) {
    for &idx in slots_order {
        if let Some(slot) = slots.get_mut(idx) {
            slot.node.recycle();
        }
    }
}

fn cancel_and_drain<N: Node>(
    cancel: &CancelToken,
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
        PassOutcome::UpstreamPending => observer.on_event(SchedulerEvent::UpstreamPending),
        PassOutcome::Backpressured => observer.on_event(SchedulerEvent::Backpressured),
        PassOutcome::Idle => observer.on_event(SchedulerEvent::Idle),
    }
}

fn park_after_outcome<N: Node, O: SchedulerObserver>(
    wake: &SchedulerWake,
    outcome: PassOutcome,
    produced_streak: &mut u32,
) {
    match outcome {
        PassOutcome::Produced => {
            *produced_streak += 1;
            if *produced_streak >= FAIRNESS_YIELD_EVERY {
                *produced_streak = 0;
                yield_now();
            }
        }
        PassOutcome::Waiting | PassOutcome::UpstreamPending | PassOutcome::Backpressured => {
            *produced_streak = 0;
            wake.wait_timeout(Scheduler::<N, O>::WAITING_TIMEOUT);
        }
        PassOutcome::Idle => {
            *produced_streak = 0;
            wake.wait_timeout(Scheduler::<N, O>::IDLE_TIMEOUT);
        }
    }
}

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

/// Forbid-blocking produce core.
///
/// The small, honest real-time region: iterate the ordered slots, tick each
/// node (decode-into-buffer + ring push), and fold the per-tick results into
/// one [`PassOutcome`]. It takes `&mut [Slot<N>]` so it can mark a slot
/// terminal in place, but must NOT grow or free the `slots`/`slots_order`
/// Vecs — that lifecycle stays in the unchecked shell. Marked
/// `#[kithara::rtsan_forbid_blocking]` so a new lock/alloc on the decode
/// path is caught; the decode subtree still carries its own permits until
/// those residuals land.
#[kithara::rtsan_forbid_blocking]
fn produce_pass<N: Node, O: SchedulerObserver>(
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
            (TickResult::UpstreamPending, _) | (_, TickResult::UpstreamPending) => {
                TickResult::UpstreamPending
            }
            (TickResult::Backpressured, _) | (_, TickResult::Backpressured) => {
                TickResult::Backpressured
            }
            _ => TickResult::Done,
        };
    }

    match best {
        TickResult::Progress => PassOutcome::Produced,
        TickResult::Waiting => PassOutcome::Waiting,
        TickResult::UpstreamPending => PassOutcome::UpstreamPending,
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

fn register_slot<N: Node>(
    slots: &mut Vec<Slot<N>>,
    needs_reorder: &mut bool,
    id: SlotId,
    mut node: N,
) {
    debug!(slot_id = id, "scheduler: registering node");
    // Shell-side, off the forbid-blocking produce core: let the node
    // pre-allocate any lazy produce-core read-path thread-local (the
    // `arc_swap` committed-read debt node) on this worker thread before its
    // first checked `tick`.
    node.warm_up();
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
    use kithara_platform::thread;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::runtime::{AtomicServiceClass, connect};

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

    /// Test node whose service class is backed by an `AtomicServiceClass`,
    /// mirroring the production `DecoderNode` — the real-time consumer writes
    /// the atomic wait-free and the scheduler reads it back each pass via
    /// `service_class()`. Lets a test drive the live re-prioritisation path
    /// (`refresh_service_classes` → `recompute_slots_order`) deterministically.
    struct AtomicClassNode {
        class: Arc<AtomicServiceClass>,
    }

    impl Node for AtomicClassNode {
        fn service_class(&self) -> ServiceClass {
            self.class.load()
        }

        fn tick(&mut self) -> TickResult {
            TickResult::Done
        }
    }

    struct FixedNode(TickResult);

    impl Node for FixedNode {
        fn tick(&mut self) -> TickResult {
            self.0
        }
    }

    fn fixed_slot(id: SlotId, result: TickResult) -> Slot<FixedNode> {
        Slot {
            id,
            node: FixedNode(result),
            service_class: ServiceClass::Audible,
            is_terminal: false,
        }
    }

    #[kithara::test]
    fn scheduler_creates_and_drops_cleanly() {
        let handle = Scheduler::<DummyNode, TestObserver>::start(
            "test-worker".into(),
            TestObserver,
            CancelToken::never(),
        );
        thread::sleep(Duration::from_millis(10)); // M5: real pacing, replace with teardown signal
        handle.shutdown();
        thread::sleep(Duration::from_millis(50)); // M5: real pacing, replace with teardown signal
    }

    #[kithara::test]
    fn scheduler_panic_isolation() {
        let handle = Scheduler::<DummyNode, TestObserver>::start(
            "test-worker".into(),
            TestObserver,
            CancelToken::never(),
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

        thread::sleep(Duration::from_millis(100)); // M5: real pacing, replace with teardown signal
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
            CancelToken::never(),
        );

        handle.register(1, BackpressureNode);

        thread::sleep(Duration::from_millis(50)); // M5: real pacing, replace with teardown signal

        let cpu_before = cpu_time_ms();
        thread::sleep(Duration::from_millis(500)); // real wall window for CPU sampling
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
    fn refresh_reorders_live_when_atomic_service_class_changes() {
        // End-to-end of the live re-prioritisation path the worker runs each
        // pass: the real-time consumer writes a track's `AtomicServiceClass`
        // wait-free; the scheduler's `refresh_service_classes` must observe the
        // new value and flag a reorder, after which `recompute_slots_order`
        // places the Audible track ahead of the Idle one. Deterministic by
        // construction — no live worker thread, no rings, no concurrency, no
        // timing: just the exact functions a pass calls, over a fixed in-memory
        // slot set with distinct classes (a total order with a unique result).
        // The strict in-pass tick ordering itself is locked separately by
        // `scheduler_orders_service_classes_descending`.
        let class_a = Arc::new(AtomicServiceClass::new(ServiceClass::Idle));
        let class_b = Arc::new(AtomicServiceClass::new(ServiceClass::Audible));

        // Slots start mislabeled (both `Idle`) so the assertion proves refresh
        // actually re-reads the node and detects the change, not that the slot
        // happened to be seeded correctly.
        let mut slots = vec![
            Slot {
                id: 0,
                node: AtomicClassNode {
                    class: Arc::clone(&class_a),
                },
                service_class: ServiceClass::Idle,
                is_terminal: false,
            },
            Slot {
                id: 1,
                node: AtomicClassNode {
                    class: Arc::clone(&class_b),
                },
                service_class: ServiceClass::Idle,
                is_terminal: false,
            },
        ];

        let mut needs_reorder = false;
        refresh_service_classes(&mut slots, &mut needs_reorder);
        assert!(
            needs_reorder,
            "refresh must flag a reorder when a node's live class differs from its slot"
        );
        assert_eq!(slots[1].service_class, ServiceClass::Audible);

        let mut order: Vec<usize> = Vec::new();
        recompute_slots_order(&slots, &mut order);
        assert_eq!(
            order,
            vec![1, 0],
            "Audible (slot 1) must be ordered before Idle (slot 0)"
        );

        // Dynamic swap: the consumer promotes A to Audible and demotes B to
        // Idle. The next pass's refresh must pick this up and the reorder must
        // flip — the property the flaky live-worker count test could never pin.
        class_a.store(ServiceClass::Audible);
        class_b.store(ServiceClass::Idle);
        needs_reorder = false;
        refresh_service_classes(&mut slots, &mut needs_reorder);
        assert!(
            needs_reorder,
            "refresh must flag a reorder after the priority swap"
        );
        recompute_slots_order(&slots, &mut order);
        assert_eq!(
            order,
            vec![0, 1],
            "after the swap, A (now Audible) must lead B (now Idle)"
        );
    }

    #[kithara::test]
    fn produce_pass_keeps_live_upstream_demand_out_of_hang_wait() {
        let mut observer = TestObserver;
        let order = vec![0usize];
        let mut pending = vec![fixed_slot(1, TickResult::UpstreamPending)];
        assert_eq!(
            produce_pass(&mut pending, &order, &mut observer),
            PassOutcome::UpstreamPending
        );

        let mut mixed = vec![
            fixed_slot(1, TickResult::UpstreamPending),
            fixed_slot(2, TickResult::Waiting),
        ];
        let order = vec![0usize, 1usize];
        assert_eq!(
            produce_pass(&mut mixed, &order, &mut observer),
            PassOutcome::Waiting,
            "a real dead upstream wait must still tick the hang detector"
        );
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

    /// A node that drains a spent-buffer inlet in `recycle` and pushes one
    /// item per `tick` into a bounded PCM ring, recording the order of
    /// recycle/produce calls and the high-water mark of un-recycled items
    /// still queued in the trash inlet.
    struct RecyclingNode {
        trash: crate::runtime::Inlet<u32>,
        pcm: crate::runtime::Outlet<usize>,
        last_call_recycle: bool,
        recycle_before_produce: bool,
        max_produce: usize,
        produced: usize,
        recycled: usize,
        trash_high_water: usize,
    }

    impl Node for RecyclingNode {
        fn recycle(&mut self) {
            let mut pending = 0usize;
            while self.trash.try_pop().is_some() {
                self.recycled += 1;
                pending += 1;
            }
            self.trash_high_water = self.trash_high_water.max(pending);
            self.last_call_recycle = true;
        }

        fn tick(&mut self) -> TickResult {
            if self.last_call_recycle {
                self.recycle_before_produce = true;
            }
            self.last_call_recycle = false;

            if self.produced >= self.max_produce {
                return TickResult::Done;
            }
            if self.pcm.try_push(self.produced).is_err() {
                return TickResult::Backpressured;
            }
            self.produced += 1;
            TickResult::Progress
        }
    }

    #[kithara::test]
    fn produce_pass_recycles_before_producing_and_never_backlogs_trash() {
        const CAP: usize = 4;
        let (mut trash_tx, trash_rx) = connect::<u32>(CAP + 2, None);
        let (pcm_tx, mut pcm_rx) = connect::<usize>(CAP, None);

        let node = RecyclingNode {
            trash: trash_rx,
            pcm: pcm_tx,
            produced: 0,
            max_produce: 64,
            recycled: 0,
            trash_high_water: 0,
            last_call_recycle: false,
            recycle_before_produce: false,
        };

        let mut slots = vec![Slot {
            id: 1,
            node,
            service_class: ServiceClass::Audible,
            is_terminal: false,
        }];
        let slots_order = vec![0usize];
        let mut observer = TestObserver;

        let mut total_spent = 0usize;
        for _ in 0..32 {
            for _ in 0..CAP {
                if trash_tx.try_push(0).is_ok() {
                    total_spent += 1;
                }
            }
            recycle_all(&mut slots, &slots_order);
            let _ = produce_pass(&mut slots, &slots_order, &mut observer);
            while pcm_rx.try_pop().is_some() {}
        }

        // Simulate a seek-drain: the audio thread returns a whole ring's
        // worth of spent buffers at once, with no produce-side consumer
        // draining in between. The trash headroom (cap+2) must absorb the
        // burst without dropping a buffer.
        let mut burst_spent = 0usize;
        while trash_tx.try_push(0).is_ok() {
            burst_spent += 1;
        }
        total_spent += burst_spent;
        assert!(
            burst_spent >= CAP + 2,
            "a full seek-drain burst must fit the trash headroom (cap+2): got {burst_spent}"
        );

        // Drain the burst the way the worker does: recycle, then let the
        // producer flush its parked overflow into the freed ring, until the
        // pipe is fully empty. No buffer is lost.
        loop {
            recycle_all(&mut slots, &slots_order);
            if !trash_tx.has_pending() {
                break;
            }
            trash_tx.flush();
        }

        let node = &slots[0].node;
        assert!(
            node.recycle_before_produce,
            "recycle_all must run before produce_pass within a pass"
        );
        assert_eq!(
            node.recycled, total_spent,
            "every spent buffer must be recycled, none leaked across passes or the seek burst"
        );
        assert!(
            node.trash_high_water >= CAP + 2,
            "the seek-drain burst (cap+2) is absorbed and recycled: high water {}",
            node.trash_high_water
        );
    }

    #[kithara::test]
    fn streak_yield_fires_only_every_n_produced_passes() {
        let wake = SchedulerWake::default();
        let mut streak = 0u32;

        for _ in 0..FAIRNESS_YIELD_EVERY - 1 {
            park_after_outcome::<DummyNode, TestObserver>(
                &wake,
                PassOutcome::Produced,
                &mut streak,
            );
        }
        assert_eq!(streak, FAIRNESS_YIELD_EVERY - 1, "streak accumulates");

        park_after_outcome::<DummyNode, TestObserver>(&wake, PassOutcome::Produced, &mut streak);
        assert_eq!(streak, 0, "streak resets to zero after the Nth yield");
    }

    #[kithara::test]
    fn non_produced_outcome_resets_streak_and_parks() {
        let wake = SchedulerWake::default();
        let mut streak = 5u32;

        wake.wake();
        park_after_outcome::<DummyNode, TestObserver>(&wake, PassOutcome::Waiting, &mut streak);
        assert_eq!(streak, 0, "Waiting resets the produced streak");

        streak = 9;
        wake.wake();
        park_after_outcome::<DummyNode, TestObserver>(
            &wake,
            PassOutcome::Backpressured,
            &mut streak,
        );
        assert_eq!(streak, 0, "Backpressured resets the produced streak");

        streak = 3;
        wake.wake();
        park_after_outcome::<DummyNode, TestObserver>(&wake, PassOutcome::Idle, &mut streak);
        assert_eq!(streak, 0, "Idle resets the produced streak");
    }
}
