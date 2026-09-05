use std::panic::{AssertUnwindSafe, catch_unwind};

use kithara_platform::{
    CancelToken,
    sync::mpsc::{self, TryRecvError},
    thread::yield_now,
    time::{Duration, Instant},
};
use kithara_test_macros as kithara;

use super::state::{Command, SchedulerBudgets, Slot};
use crate::{Event, Observer, PassOutcome, PassReport, TaskId, TickResult, Wake};

#[kithara::flash(true)]
pub(super) fn run_loop(
    cmd_rx: &mpsc::Receiver<Command>,
    wake: &Wake,
    cancel: &CancelToken,
    budgets: SchedulerBudgets,
    mut observer: Box<dyn Observer>,
) {
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
        cancel_cancelled(&mut slots);
        needs_reorder |= remove_terminal(&mut slots);
        refresh_priorities(&mut slots, &mut needs_reorder);
        if needs_reorder {
            reorder_slots(&mut slots);
            needs_reorder = false;
        }
        recycle_all(&mut slots);

        let report = produce_pass(&mut slots, budgets, observer.as_mut());
        needs_reorder |= remove_terminal(&mut slots);
        report_outcome(observer.as_mut(), report);
        observer.on_event(Event::PassEnd);
        park_after_outcome(wake, budgets, report, &mut progress_streak);
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
                    slot.priority = slot.control.priority();
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
        let priority = slot.control.priority();
        if priority != slot.priority {
            slot.priority = priority;
            *needs_reorder = true;
        }
    }
}

pub(super) fn reorder_slots(slots: &mut [Slot]) {
    for index in 1..slots.len() {
        let mut position = index;
        while position > 0 && slot_precedes(&slots[position], &slots[position - 1]) {
            slots.swap(position - 1, position);
            position -= 1;
        }
    }
}

fn slot_precedes(left: &Slot, right: &Slot) -> bool {
    left.priority > right.priority || (left.priority == right.priority && left.id < right.id)
}

pub(super) fn produce_pass(
    slots: &mut [Slot],
    budgets: SchedulerBudgets,
    observer: &mut dyn Observer,
) -> PassReport {
    let mut report = PassReport::new(slots.len());
    let mut best = TickResult::Done;

    for slot in &mut *slots {
        let visit_start = Instant::now();
        let mut last = TickResult::Progress;
        let mut progressed = false;

        for tick in 0..budgets.task_burst {
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
            if elapsed > budgets.slow_tick_threshold {
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
    budgets: SchedulerBudgets,
    report: PassReport,
    progress_streak: &mut u32,
) {
    match report.outcome {
        PassOutcome::Progress => {
            *progress_streak += 1;
            if *progress_streak >= budgets.fairness_yield_interval {
                *progress_streak = 0;
                yield_now();
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
fn wait_for_backpressure(wake: &Wake, budgets: SchedulerBudgets) {
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
            woken = wake.wait_timeout(wait.min(watchdog_remaining));
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
