use std::{
    mem,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_platform::{
    CancelGroup, CancelScope,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use kithara_test_utils::{hang::default_timeout, kithara, probe::capture as probe_capture};

use super::{
    TaskError,
    core::{
        cancel_all, park_after_outcome, produce_pass, recycle_all, refresh_priorities,
        remove_terminal, reorder_slots, unregister_slot,
    },
    state::{SchedulerBudgets, Slot},
};
use crate::{
    DispatcherConfig, Event, Observer, PassOutcome, PassReport, Priority, Task, TaskConfig,
    TaskControl, TaskId, TickResult, Wake,
};

struct FixedTask(TickResult);

impl Task for FixedTask {
    fn tick(&mut self) -> TickResult {
        self.0
    }
}

struct CountingTask {
    ticks: Arc<AtomicUsize>,
}

impl Task for CountingTask {
    fn tick(&mut self) -> TickResult {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        TickResult::Progress
    }
}

struct BackpressureCountingTask {
    ticks: Arc<AtomicUsize>,
    first_tick: Option<mpsc::Sender<()>>,
}

impl Task for BackpressureCountingTask {
    fn tick(&mut self) -> TickResult {
        if let Some(first_tick) = self.first_tick.take() {
            first_tick.send(()).ok();
        }
        self.ticks.fetch_add(1, Ordering::Relaxed);
        TickResult::Backpressured
    }
}

struct BlockingTask {
    started: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
}

impl Task for BlockingTask {
    fn tick(&mut self) -> TickResult {
        if let Some(started) = self.started.take() {
            started.send(()).ok();
        }
        self.release.recv().ok();
        TickResult::Done
    }
}

struct LifecycleTask {
    events: mpsc::Sender<&'static str>,
}

impl Task for LifecycleTask {
    fn on_cancel(&mut self) {
        self.events.send("cancel").ok();
    }

    fn recycle(&mut self) {
        self.events.send("recycle").ok();
    }

    fn tick(&mut self) -> TickResult {
        self.events.send("tick").ok();
        TickResult::Done
    }
}

struct DeferredWaitingTask {
    events: mpsc::Sender<&'static str>,
    pending: bool,
}

impl Task for DeferredWaitingTask {
    fn recycle(&mut self) {
        if mem::take(&mut self.pending) {
            self.events.send("recycle").ok();
        }
    }

    fn tick(&mut self) -> TickResult {
        self.pending = true;
        TickResult::Waiting
    }
}

struct RecyclingTask {
    events: mpsc::Sender<&'static str>,
    ticks: usize,
}

impl Task for RecyclingTask {
    fn recycle(&mut self) {
        self.events.send("recycle").ok();
    }

    fn tick(&mut self) -> TickResult {
        self.events.send("tick").ok();
        self.ticks += 1;
        if self.ticks == 4 {
            TickResult::Done
        } else {
            TickResult::Progress
        }
    }
}

struct TerminalDeferredTask {
    flushed: Arc<AtomicBool>,
    pending: bool,
}

impl Task for TerminalDeferredTask {
    fn recycle(&mut self) {
        if mem::take(&mut self.pending) {
            self.flushed.store(true, Ordering::Release);
        }
    }

    fn tick(&mut self) -> TickResult {
        self.pending = true;
        TickResult::Done
    }
}

struct ReportingTask {
    label: &'static str,
    events: mpsc::Sender<(&'static str, &'static str, u64)>,
    ticked: bool,
}

impl Task for ReportingTask {
    fn on_cancel(&mut self) {
        self.events
            .send((self.label, "cancel", thread::current_thread_id()))
            .ok();
    }

    fn tick(&mut self) -> TickResult {
        if !self.ticked {
            self.ticked = true;
            self.events
                .send((self.label, "tick", thread::current_thread_id()))
                .ok();
        }
        TickResult::Backpressured
    }
}

struct PanicTask;

impl Task for PanicTask {
    fn tick(&mut self) -> TickResult {
        panic!("test task panic");
    }
}

struct PanicObserver {
    panicked: mpsc::Sender<TaskId>,
}

impl Observer for PanicObserver {
    fn on_event(&mut self, event: Event) {
        if let Event::TaskPanicked { task } = event {
            self.panicked.send(task).ok();
        }
    }
}

#[derive(Default)]
struct Events(Vec<Event>);

impl Observer for Events {
    fn on_event(&mut self, event: Event) {
        self.0.push(event);
    }
}

fn budgets() -> SchedulerBudgets {
    SchedulerBudgets {
        backpressure_poll_interval: Duration::from_millis(10),
        fairness_yield_interval: 16,
        idle_timeout: Duration::from_millis(100),
        slow_tick_threshold: Duration::from_secs(1),
        task_burst: 32,
        wait_timeout: Duration::from_millis(10),
    }
}

fn pass_report(outcome: PassOutcome) -> PassReport {
    let mut report = PassReport::new(0);
    report.outcome = outcome;
    report
}

fn slot(id: u64, priority: Priority, task: impl Task) -> Slot {
    let scope = CancelScope::new(None);
    let token = scope.token().child();

    Slot {
        _cancel_guards: Vec::new(),
        cancel: CancelGroup::from(token.clone()),
        control: TaskControl::new(priority, token.clone(), Wake::default()),
        id: TaskId::new(id),
        is_terminal: false,
        priority,
        task: Box::new(task),
        token,
    }
}

#[kithara::test(native, flash(false))]
fn numeric_priority_is_descending_with_stable_id_tie_break() {
    let mut slots = vec![
        slot(3, Priority::new(5), FixedTask(TickResult::Done)),
        slot(2, Priority::new(8), FixedTask(TickResult::Done)),
        slot(1, Priority::new(8), FixedTask(TickResult::Done)),
    ];

    reorder_slots(&mut slots);

    assert_eq!(
        slots.iter().map(|slot| slot.id.get()).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[kithara::test(native, flash(false))]
fn priority_control_refreshes_the_single_mutable_priority_source() {
    let mut slots = vec![
        slot(1, Priority::new(1), FixedTask(TickResult::Done)),
        slot(2, Priority::new(2), FixedTask(TickResult::Done)),
    ];
    slots[0].control.set_priority(Priority::new(3));
    let mut needs_reorder = false;

    refresh_priorities(&mut slots, &mut needs_reorder);
    reorder_slots(&mut slots);

    assert!(needs_reorder);
    assert_eq!(slots[0].id, TaskId::new(1));
}

#[kithara::test(native, flash(false))]
fn configured_task_burst_limits_one_visit() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let mut slots = vec![slot(
        1,
        Priority::default(),
        CountingTask {
            ticks: Arc::clone(&ticks),
        },
    )];
    let mut observer = Events::default();
    let mut configured = budgets();
    configured.task_burst = 2;

    let report = produce_pass(&mut slots, configured, &mut observer);

    assert_eq!(report.outcome, PassOutcome::Progress);
    assert_eq!(ticks.load(Ordering::Relaxed), 2);
}

#[kithara::test(native, flash(false))]
fn configured_slow_threshold_and_fairness_interval_are_load_bearing() {
    let mut slots = vec![slot(1, Priority::default(), FixedTask(TickResult::Done))];
    let mut observer = Events::default();
    let mut configured = budgets();
    configured.slow_tick_threshold = Duration::ZERO;

    let _ = produce_pass(&mut slots, configured, &mut observer);
    assert!(
        observer
            .0
            .iter()
            .any(|event| matches!(event, Event::SlowTick { .. }))
    );

    configured.fairness_yield_interval = 1;
    let mut streak = 0;
    park_after_outcome(
        &Wake::default(),
        configured,
        pass_report(PassOutcome::Progress),
        &mut streak,
    );
    assert_eq!(streak, 0);
}

#[kithara::test(native, flash(false))]
fn scheduler_does_not_busy_spin_on_backpressure() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let (first_tick, first_tick_rx) = mpsc::channel();
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("backpressure-park-test")
            .backpressure_poll_interval(Duration::from_millis(1))
            .wait_timeout(Duration::from_millis(20))
            .build(),
    );
    let handle = dispatcher
        .register(TaskConfig::new(), {
            let ticks = Arc::clone(&ticks);
            move |_| BackpressureCountingTask {
                ticks,
                first_tick: Some(first_tick),
            }
        })
        .expect("backpressured task submission");

    first_tick_rx
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("backpressured task must start");
    thread::sleep(Duration::from_millis(80));

    let observed = ticks.load(Ordering::Relaxed);
    drop(handle);
    assert!(
        observed < 16,
        "backpressured task ran {observed} times in 80ms despite a 20ms park budget"
    );
}

#[kithara::test(native, flash(false))]
fn produce_pass_keeps_live_upstream_demand_out_of_waiting_outcome() {
    let mut observer = Events::default();
    let mut pending = vec![slot(
        1,
        Priority::default(),
        FixedTask(TickResult::UpstreamPending),
    )];
    assert_eq!(
        produce_pass(&mut pending, budgets(), &mut observer).outcome,
        PassOutcome::UpstreamPending
    );

    let mut mixed = vec![
        slot(
            1,
            Priority::default(),
            FixedTask(TickResult::UpstreamPending),
        ),
        slot(2, Priority::default(), FixedTask(TickResult::Waiting)),
    ];
    assert_eq!(
        produce_pass(&mut mixed, budgets(), &mut observer).outcome,
        PassOutcome::Waiting,
        "a real upstream wait must still outrank live pending demand"
    );
}

#[kithara::test(native, flash(false))]
fn terminal_visit_recycles_before_and_after_tick() {
    let (events, received) = mpsc::channel();
    let mut slots = vec![slot(1, Priority::default(), LifecycleTask { events })];
    let mut observer = Events::default();

    recycle_all(&mut slots);
    let report = produce_pass(&mut slots, budgets(), &mut observer);

    assert_eq!(report.outcome, PassOutcome::Idle);
    assert!(slots[0].is_terminal);
    assert_eq!(received.try_recv(), Ok("recycle"));
    assert_eq!(received.try_recv(), Ok("tick"));
    assert_eq!(received.try_recv(), Ok("recycle"));
    assert_eq!(received.try_recv(), Ok("recycle"));
    assert!(received.try_recv().is_err());
    assert!(remove_terminal(&mut slots));
    assert!(slots.is_empty());
}

#[kithara::test(native, flash(false))]
fn live_visit_flushes_deferred_work_before_reporting_wait() {
    let (events, received) = mpsc::channel();
    let mut slots = vec![slot(
        1,
        Priority::default(),
        DeferredWaitingTask {
            events,
            pending: false,
        },
    )];
    let mut observer = Events::default();

    let report = produce_pass(&mut slots, budgets(), &mut observer);

    assert_eq!(report.outcome, PassOutcome::Waiting);
    assert_eq!(received.try_recv(), Ok("recycle"));
    assert!(received.try_recv().is_err());
}

#[kithara::test(native, flash(false))]
fn unregister_recycles_cancelled_task_before_removal() {
    let (events, received) = mpsc::channel();
    let mut slots = vec![slot(1, Priority::default(), LifecycleTask { events })];
    let mut needs_reorder = false;

    unregister_slot(&mut slots, &mut needs_reorder, TaskId::new(1));

    assert!(slots.is_empty());
    assert!(needs_reorder);
    assert_eq!(received.try_recv(), Ok("cancel"));
    assert_eq!(received.try_recv(), Ok("recycle"));
    assert!(received.try_recv().is_err());
}

#[kithara::test(native, flash(false))]
fn shutdown_recycles_cancelled_tasks_before_drop() {
    let (events, received) = mpsc::channel();
    let mut slots = vec![slot(1, Priority::default(), LifecycleTask { events })];

    cancel_all(&mut slots);

    assert!(slots[0].is_terminal);
    assert_eq!(received.try_recv(), Ok("cancel"));
    assert_eq!(received.try_recv(), Ok("recycle"));
    assert!(received.try_recv().is_err());
}

#[kithara::test(native, flash(false))]
fn shutdown_cancels_and_recycles_a_queued_never_run_task_once() {
    let recorder = probe_capture::install();
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("queued-shutdown-test")
            .build(),
    );
    let (started, started_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let blocker = dispatcher
        .register(TaskConfig::new(), move |_| BlockingTask {
            release: release_rx,
            started: Some(started),
        })
        .expect("blocking task submission");
    started_rx
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("blocking task must hold the dispatcher");
    let (events, received) = mpsc::channel();
    let queued = dispatcher
        .register(TaskConfig::new(), move |_| LifecycleTask { events })
        .expect("queued task submission");

    dispatcher.shutdown();
    release.send(()).expect("release blocking task");

    recorder
        .wait_for_probe(
            |event| {
                event.probe_name() == Some("cancel")
                    && event.u64("task_id") == Some(queued.id().get())
                    && event.u64("already_terminal") == Some(0)
            },
            default_timeout(),
        )
        .expect("queued task cancellation probe");

    assert_eq!(
        received
            .recv_timeout(Instant::now() + default_timeout())
            .expect("queued task cancellation"),
        "cancel"
    );
    assert_eq!(
        received
            .recv_timeout(Instant::now() + default_timeout())
            .expect("queued task recycle"),
        "recycle"
    );
    assert!(received.try_recv().is_err());
    drop(blocker);
    drop(queued);
}

#[kithara::test(native, flash(false))]
fn shutdown_closes_task_admission_before_returning() {
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("closed-admission-test")
            .build(),
    );

    dispatcher.shutdown();

    assert_eq!(
        dispatcher
            .register(TaskConfig::new(), |_| FixedTask(TickResult::Done))
            .err(),
        Some(TaskError::Stopped)
    );
}

#[kithara::test(native, flash(false))]
fn produce_pass_recycles_before_producing_and_between_burst_ticks() {
    let (events, received) = mpsc::channel();
    let mut slots = vec![slot(
        1,
        Priority::default(),
        RecyclingTask { events, ticks: 0 },
    )];
    let mut observer = Events::default();

    recycle_all(&mut slots);
    let report = produce_pass(&mut slots, budgets(), &mut observer);

    assert_eq!(report.outcome, PassOutcome::Idle);
    let sequence = std::iter::from_fn(|| received.try_recv().ok()).collect::<Vec<_>>();
    assert_eq!(
        sequence,
        vec![
            "recycle", "tick", "recycle", "tick", "recycle", "tick", "recycle", "tick", "recycle",
            "recycle",
        ]
    );
}

#[kithara::test(native, flash(false))]
fn terminal_tick_flushes_deferred_work_before_slot_removal() {
    let flushed = Arc::new(AtomicBool::new(false));
    let mut slots = vec![slot(
        1,
        Priority::default(),
        TerminalDeferredTask {
            flushed: Arc::clone(&flushed),
            pending: false,
        },
    )];
    let mut observer = Events::default();

    let report = produce_pass(&mut slots, budgets(), &mut observer);

    assert_eq!(report.outcome, PassOutcome::Idle);
    assert!(slots[0].is_terminal);
    assert!(flushed.load(Ordering::Acquire));
}

#[kithara::test(native, flash(false))]
fn fairness_streak_yields_at_the_configured_interval_and_resets_on_waits() {
    let wake = Wake::default();
    let mut configured = budgets();
    configured.fairness_yield_interval = 3;
    configured.backpressure_poll_interval = Duration::ZERO;
    configured.idle_timeout = Duration::ZERO;
    configured.wait_timeout = Duration::ZERO;
    let mut streak = 0;

    park_after_outcome(
        &wake,
        configured,
        pass_report(PassOutcome::Progress),
        &mut streak,
    );
    park_after_outcome(
        &wake,
        configured,
        pass_report(PassOutcome::Progress),
        &mut streak,
    );
    assert_eq!(streak, 2);
    park_after_outcome(
        &wake,
        configured,
        pass_report(PassOutcome::Progress),
        &mut streak,
    );
    assert_eq!(streak, 0);

    for outcome in [
        PassOutcome::Waiting,
        PassOutcome::UpstreamPending,
        PassOutcome::Backpressured,
        PassOutcome::Idle,
    ] {
        streak = 2;
        park_after_outcome(&wake, configured, pass_report(outcome), &mut streak);
        assert_eq!(streak, 0, "{outcome:?} must reset the progress streak");
    }
}

#[kithara::test(native, flash(false))]
fn backpressure_poll_slices_wait_for_a_deferred_edge_without_restarting_the_task() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let (first_tick, first_tick_rx) = mpsc::channel();
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("backpressure-deferred-poll-test")
            .backpressure_poll_interval(Duration::from_millis(2))
            .wait_timeout(Duration::from_millis(500))
            .build(),
    );
    let handle = dispatcher
        .register(TaskConfig::new(), {
            let ticks = Arc::clone(&ticks);
            move |_| BackpressureCountingTask {
                first_tick: Some(first_tick),
                ticks,
            }
        })
        .expect("backpressured task submission");

    first_tick_rx
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("backpressured task must start");
    thread::sleep(Duration::from_millis(20));
    let settled_ticks = ticks.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(
        ticks.load(Ordering::Relaxed),
        settled_ticks,
        "pure poll-slice timeouts must not restart the task"
    );

    handle.control().defer();
    let deadline = Instant::now() + Duration::from_millis(100);
    while ticks.load(Ordering::Relaxed) == settled_ticks && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        ticks.load(Ordering::Relaxed),
        settled_ticks + 1,
        "a deferred edge must end sliced backpressure waiting before the liveness timeout"
    );
}

#[kithara::test(native, flash(false))]
fn configured_capacity_rejects_a_second_reservation() {
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("capacity-test")
            .capacity(NonZeroUsize::MIN)
            .build(),
    );
    let first = dispatcher
        .reserve(TaskConfig::new())
        .expect("first reserve");

    assert_eq!(
        dispatcher.reserve(TaskConfig::new()).err(),
        Some(TaskError::Capacity { capacity: 1 })
    );
    drop(first);
    assert!(dispatcher.reserve(TaskConfig::new()).is_ok());
}

#[kithara::test(native, flash(false))]
fn task_handle_drop_releases_capacity_for_immediate_ordered_replacement() {
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("replacement-test")
            .capacity(NonZeroUsize::MIN)
            .build(),
    );
    let first = dispatcher
        .register(TaskConfig::new(), |_| FixedTask(TickResult::Backpressured))
        .expect("first task");

    drop(first);

    assert!(
        dispatcher
            .register(TaskConfig::new(), |_| {
                FixedTask(TickResult::Backpressured)
            })
            .is_ok()
    );
}

#[kithara::test(native, flash(false))]
fn dispatcher_builder_uses_scheduler_defaults() {
    let config = DispatcherConfig::builder().name("defaults-test").build();

    assert_eq!(config.backpressure_poll_interval, Duration::from_millis(10));
    assert_eq!(config.capacity.get(), 64);
    assert_eq!(config.fairness_yield_interval.get(), 16);
    assert_eq!(config.idle_timeout, Duration::from_millis(100));
    assert_eq!(config.slow_tick_threshold, Duration::from_millis(10));
    assert_eq!(config.task_burst.get(), 32);
    assert_eq!(config.wait_timeout, Duration::from_millis(10));
    assert!(config.cancel.is_none());
}

#[kithara::test(native, flash(false))]
fn non_zero_budget_types_are_accepted_by_the_dispatcher_builder() {
    let _ = DispatcherConfig::builder()
        .name("budget-test")
        .backpressure_poll_interval(Duration::ZERO)
        .capacity(NonZeroUsize::MIN)
        .fairness_yield_interval(NonZeroU32::MIN)
        .idle_timeout(Duration::ZERO)
        .slow_tick_threshold(Duration::ZERO)
        .task_burst(NonZeroU32::MIN)
        .wait_timeout(Duration::ZERO)
        .build();
}

#[kithara::test(native, flash(false))]
fn dispatcher_cancel_group_does_not_cancel_worker_or_sibling_dispatcher() {
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let domain = CancelScope::new(None);
    let first = worker.dispatcher(
        DispatcherConfig::builder()
            .name("first-dispatcher")
            .cancel(CancelGroup::from(domain.token()))
            .build(),
    );
    let sibling = worker.dispatcher(
        DispatcherConfig::builder()
            .name("sibling-dispatcher")
            .build(),
    );

    domain.cancel();

    assert!(first.is_cancelled());
    assert!(!sibling.is_cancelled());
    assert!(!worker.is_cancelled());
}

#[kithara::test(native, flash(false))]
fn external_cancel_is_task_local_and_control_cancel_uses_dispatcher_thread() {
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("task-cancel-test")
            .wait_timeout(Duration::from_secs(30))
            .build(),
    );
    let domain = CancelScope::new(None);
    let (events, received) = mpsc::channel();
    let first = dispatcher
        .reserve(
            TaskConfig::new()
                .with_cancel(CancelGroup::from(domain.token()))
                .with_priority(Priority::new(2)),
        )
        .expect("first task reservation");
    let first_context = first.context().clone();
    let first_handle = first
        .start({
            let events = events.clone();
            move |_| ReportingTask {
                events,
                label: "first",
                ticked: false,
            }
        })
        .expect("first task submission");
    let second = dispatcher
        .reserve(TaskConfig::new().with_priority(Priority::new(1)))
        .expect("second task reservation");
    let second_context = second.context().clone();
    let second_handle = second
        .start(move |_| ReportingTask {
            events,
            label: "second",
            ticked: false,
        })
        .expect("second task submission");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut first_thread = None;
    let mut second_thread = None;
    while first_thread.is_none() || second_thread.is_none() {
        let (label, event, thread) = received
            .recv_timeout(deadline)
            .expect("both tasks must tick");
        if event == "tick" && label == "first" {
            first_thread = Some(thread);
        } else if event == "tick" && label == "second" {
            second_thread = Some(thread);
        }
    }

    domain.cancel();
    let (label, event, cancel_thread) = received
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("external cancellation must wake the parked dispatcher");
    assert_eq!((label, event), ("first", "cancel"));
    assert_eq!(Some(cancel_thread), first_thread);
    assert!(first_context.token().is_cancelled());
    assert!(!second_context.token().is_cancelled());
    assert!(!dispatcher.is_cancelled());
    assert!(!worker.is_cancelled());

    second_handle.control().cancel();
    let (label, event, cancel_thread) = received
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("task control cancellation must wake the dispatcher");
    assert_eq!((label, event), ("second", "cancel"));
    assert_eq!(Some(cancel_thread), second_thread);
    assert!(second_context.token().is_cancelled());

    drop(first_handle);
    drop(second_handle);
}

#[kithara::test(native, flash(false))]
fn a_panicking_task_does_not_stop_its_sibling() {
    let worker = crate::Worker::new(crate::WorkerConfig::new());
    let (panicked, observed_panic) = mpsc::channel();
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("panic-isolation-test")
            .observer(PanicObserver { panicked })
            .build(),
    );
    let panic_handle = dispatcher
        .register(TaskConfig::new(), |_| PanicTask)
        .expect("panic task submission");
    let (events, received) = mpsc::channel();
    let sibling_handle = dispatcher
        .register(TaskConfig::new(), move |_| ReportingTask {
            events,
            label: "sibling",
            ticked: false,
        })
        .expect("sibling task submission");

    assert_eq!(
        observed_panic
            .recv_timeout(Instant::now() + Duration::from_secs(2))
            .expect("panic event"),
        panic_handle.id()
    );
    let (label, event, _) = received
        .recv_timeout(Instant::now() + Duration::from_secs(2))
        .expect("sibling must still tick");
    assert_eq!((label, event), ("sibling", "tick"));

    drop(panic_handle);
    drop(sibling_handle);
}
