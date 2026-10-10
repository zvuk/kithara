use std::panic::{AssertUnwindSafe, catch_unwind};

use kithara_config::{Config, ConfigOwner};
use kithara_platform::{
    CancelToken,
    sync::mpsc::{self, TryRecvError},
    thread::{self, ThreadClass, yield_runnable},
    time::{Duration, Instant},
};
use kithara_test_macros as kithara;

use super::state::{Command, Slot};
use crate::{DispatcherConfig, Event, Observer, PassOutcome, PassReport, TaskId, TickResult, Wake};

#[kithara::flash(true)]
pub(super) fn run_loop(
    cmd_rx: &mpsc::Receiver<Command>,
    wake: &Wake,
    cancel: &CancelToken,
    budgets: &DispatcherConfig,
    mut observer: Box<dyn Observer>,
) {
    claim_thread_class(budgets.thread_class, observer.as_mut());
    let mut slots = Vec::new();
    let mut needs_reorder = false;
    let mut progress_streak = 0;

    loop {
        observer.on_event(Event::PassStart);
        if cancel_and_drain(
            cancel,
            cmd_rx,
            &mut slots,
            &mut needs_reorder,
            observer.as_mut(),
        ) {
            return;
        }
        let report = run_pass(&mut slots, &mut needs_reorder, budgets, observer.as_mut());
        observer.on_event(Event::PassEnd);
        park_after_outcome(wake, budgets, report, &mut progress_streak);
    }
}

/// Ask the OS for the dispatcher's thread class before the first pass, so no
/// task ever runs on a thread scheduled below what its work needs.
fn claim_thread_class(class: ThreadClass, observer: &mut dyn Observer) {
    let refused = thread::set_current_class(class)
        .err()
        .map(|error| error.kind());
    kithara::probe_event!(
        thread_class,
        audio_feed = u64::from(class == ThreadClass::AudioFeed),
        granted = u64::from(refused.is_none())
    );
    if let Some(error) = refused {
        observer.on_event(Event::ThreadClassRefused { class, error });
    }
}

/// One scheduling pass: settle the roster the commands left behind, produce
/// work from it, and report what the pass achieved.
///
/// Leaves no terminal slot behind for the next pass to park on, and preserves the existing order of
/// whatever slots remain.
/// The probe carries the first backpressured task so repeated identical counts
/// distinguish one stuck holder from shifting holders without another reading.
/// USDT's five payload slots replace `done`: finished tasks unregister in that pass,
/// so repeating passes have none. The hang detector supplies the waiting task's route.
pub(super) fn run_pass(
    slots: &mut Vec<Slot>,
    needs_reorder: &mut bool,
    budgets: &DispatcherConfig,
    observer: &mut dyn Observer,
) -> PassReport {
    cancel_cancelled(slots);
    *needs_reorder |= remove_terminal(slots);
    refresh_priorities(slots, needs_reorder);
    if *needs_reorder {
        reorder_slots(slots);
        *needs_reorder = false;
    }
    recycle_all(slots);

    let report = produce_pass(slots, budgets, observer);
    kithara::probe_event!(
        scheduler_pass,
        active = report.active_tasks,
        progress = report.progress_tasks,
        waiting = report.waiting_tasks,
        backpressured = report.backpressured_tasks,
        first_backpressured = task_field(report.first_backpressured_task)
    );
    remove_terminal(slots);
    report_outcome(observer, report);
    report
}

/// A task identifier as a probe field, with `0` standing for no task.
///
/// Identifiers are handed out from one upwards, so zero names nothing and a
/// probe reader needs no companion flag to tell an absent task from task one.
const fn task_field(task: Option<TaskId>) -> u64 {
    match task {
        Some(task) => task.get(),
        None => 0,
    }
}

fn cancel_and_drain(
    cancel: &CancelToken,
    cmd_rx: &mpsc::Receiver<Command>,
    slots: &mut Vec<Slot>,
    needs_reorder: &mut bool,
    observer: &mut dyn Observer,
) -> bool {
    let shutdown = drain_commands(cmd_rx, slots, needs_reorder, observer);
    if cancel.is_cancelled() {
        cancel_all(slots);
        return true;
    }
    shutdown
}

fn drain_commands(
    cmd_rx: &mpsc::Receiver<Command>,
    slots: &mut Vec<Slot>,
    needs_reorder: &mut bool,
    observer: &mut dyn Observer,
) -> bool {
    loop {
        match cmd_rx.try_recv() {
            Ok(Command::Register(registration)) => {
                let mut slot = match registration.build_slot() {
                    Ok(slot) => slot,
                    Err(id) => {
                        observer.on_event(Event::TaskPanicked { task: id });
                        continue;
                    }
                };
                if slot.cancel.is_cancelled() {
                    cancel_slot(&mut slot);
                } else {
                    slot.task.warm_up();
                    slots.push(slot);
                    *needs_reorder = true;
                }
            }
            Ok(Command::Unregister(id)) => unregister_slot(slots, needs_reorder, id),
            Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                cancel_all(slots);
                return true;
            }
            Err(TryRecvError::Empty) => return false,
            #[cfg(target_arch = "wasm32")]
            Err(_) => return false,
        }
    }
}

pub(super) fn unregister_slot(slots: &mut Vec<Slot>, needs_reorder: &mut bool, id: TaskId) {
    if let Some(slot) = slots.iter_mut().find(|slot| slot.id == id) {
        cancel_slot(slot);
    }
    *needs_reorder |= remove_terminal(slots);
}

fn cancel_cancelled(slots: &mut [Slot]) {
    for slot in slots {
        if slot.cancel.is_cancelled() {
            cancel_slot(slot);
        }
    }
}

pub(super) fn cancel_all(slots: &mut [Slot]) {
    for slot in slots {
        cancel_slot(slot);
    }
}

fn cancel_slot(slot: &mut Slot) {
    slot.cancel();
}

pub(super) fn recycle_all(slots: &mut [Slot]) {
    for slot in slots {
        slot.task.recycle();
    }
}

pub(super) fn remove_terminal(slots: &mut Vec<Slot>) -> bool {
    let before = slots.len();
    slots.retain(|slot| !slot.is_terminal);
    slots.len() < before
}

pub(super) fn refresh_priorities(slots: &mut [Slot], needs_reorder: &mut bool) {
    for slot in slots {
        let priority = slot
            .task
            .priority()
            .unwrap_or_else(|| slot.control.config().values().priority);
        if priority != slot.priority {
            slot.priority = priority;
            *needs_reorder = true;
        }
    }
}

pub(super) fn reorder_slots(slots: &mut [Slot]) {
    slots.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then(left.id.cmp(&right.id))
    });
}

pub(super) fn produce_pass(
    slots: &mut [Slot],
    budgets: &DispatcherConfig,
    observer: &mut dyn Observer,
) -> PassReport {
    let mut report = PassReport::new(slots.len());
    let mut best = TickResult::Done;

    for slot in &mut *slots {
        let visit_start = Instant::now();
        let mut last = TickResult::Progress;
        let mut progressed = false;

        for tick in 0..budgets.task_burst.get() {
            if tick > 0 {
                slot.task.recycle();
            }
            if slot.cancel.is_cancelled() {
                cancel_slot(slot);
                last = TickResult::Done;
                break;
            }

            let start = Instant::now();
            last = if let Ok(result) = catch_unwind(AssertUnwindSafe(|| slot.task.tick())) {
                result
            } else {
                observer.on_event(Event::TaskPanicked { task: slot.id });
                cancel_slot(slot);
                TickResult::Done
            };
            let elapsed = start.elapsed();
            if is_slow_tick(elapsed, budgets.slow_tick_threshold) {
                observer.on_event(Event::SlowTick {
                    elapsed,
                    task: slot.id,
                });
            }
            if last != TickResult::Progress {
                break;
            }
            progressed = true;
            if visit_start.elapsed() >= budgets.slow_tick_threshold {
                break;
            }
        }

        slot.task.recycle();
        let result = match last {
            TickResult::Done => TickResult::Done,
            _ if progressed => TickResult::Progress,
            other => other,
        };
        report.record(slot.id, slot.priority, result);
        if result == TickResult::Done {
            slot.is_terminal = true;
        }
        best = best_result(best, result);
    }

    recycle_all(slots);
    report.outcome = match best {
        TickResult::Progress => PassOutcome::Progress,
        TickResult::Waiting => PassOutcome::Waiting,
        TickResult::UpstreamPending => PassOutcome::UpstreamPending,
        TickResult::Backpressured => PassOutcome::Backpressured,
        TickResult::Done => PassOutcome::Idle,
    };
    report
}

/// A tick is slow once it costs more than the budget it was given; one that
/// lands exactly on the budget spent no more than it was allowed to.
pub(super) const fn is_slow_tick(elapsed: Duration, threshold: Duration) -> bool {
    elapsed.as_nanos() > threshold.as_nanos()
}

fn best_result(current: TickResult, next: TickResult) -> TickResult {
    match (current, next) {
        (TickResult::Progress, _) | (_, TickResult::Progress) => TickResult::Progress,
        (TickResult::Waiting, _) | (_, TickResult::Waiting) => TickResult::Waiting,
        (TickResult::UpstreamPending, _) | (_, TickResult::UpstreamPending) => {
            TickResult::UpstreamPending
        }
        (TickResult::Backpressured, _) | (_, TickResult::Backpressured) => {
            TickResult::Backpressured
        }
        (TickResult::Done, TickResult::Done) => TickResult::Done,
    }
}

fn report_outcome(observer: &mut dyn Observer, report: PassReport) {
    observer.on_event(match report.outcome {
        PassOutcome::Progress => Event::Progress(report),
        PassOutcome::Waiting => Event::Waiting(report),
        PassOutcome::UpstreamPending => Event::UpstreamPending(report),
        PassOutcome::Backpressured => Event::Backpressured(report),
        PassOutcome::Idle => Event::Idle(report),
    });
}

pub(super) fn park_after_outcome(
    wake: &Wake,
    budgets: &DispatcherConfig,
    report: PassReport,
    progress_streak: &mut u32,
) {
    match report.outcome {
        PassOutcome::Progress => {
            *progress_streak += 1;
            if *progress_streak >= budgets.fairness_yield_interval.get() {
                *progress_streak = 0;
                yield_runnable();
            }
        }
        PassOutcome::Waiting | PassOutcome::UpstreamPending | PassOutcome::Backpressured => {
            *progress_streak = 0;
            if report.backpressured_tasks > 0 {
                wait_for_backpressure(wake, budgets);
            } else {
                wake.wait_timeout(budgets.wait_timeout);
            }
        }
        PassOutcome::Idle => {
            *progress_streak = 0;
            wake.wait_timeout(budgets.idle_timeout);
        }
    }
}

#[kithara::measure(label = "worker.backpressure.wait")]
#[kithara::hang_watchdog]
fn wait_for_backpressure(wake: &Wake, budgets: &DispatcherConfig) {
    let poll_interval = budgets.backpressure_poll_interval;
    let deadline = budgets.wait_timeout;
    if poll_interval.is_zero() || deadline.is_zero() {
        wake.wait_timeout(Duration::ZERO);
        return;
    }

    let started = Instant::now();
    let mut remaining = deadline;
    loop {
        let wait = poll_interval.min(remaining);
        let mut woken = false;
        hang_park!(|watchdog_remaining| {
            woken = wake.wait_poll_timeout(wait.min(watchdog_remaining));
        });
        if woken {
            return;
        }
        remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return;
        }
    }
}
