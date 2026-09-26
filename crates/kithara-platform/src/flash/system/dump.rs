use std::fmt;

use super::{
    FlashInner, Registry,
    sched::{Entry, WaitKind},
    wake::Wake,
};

/// Append the diagnostic detail of one parked waiter to `f`: the real async
/// primitive behind a `Condvar(CvId)` waiter (kind + creation site, recorded by
/// [`FlashInner::describe_cvid`](super::FlashInner::describe_cvid)) and the async
/// task parked on it (id + spawn site, captured at registration). Both are
/// best-effort — absent when sync tracing was off at construction / no task
/// context was active.
fn write_waiter_detail(
    f: &mut fmt::Formatter<'_>,
    kind: &WaitKind,
    wake: &Wake,
    reg: &Registry,
) -> fmt::Result {
    if let WaitKind::Condvar(cvid) = kind
        && let Some(d) = reg.cv_desc.get(&cvid.0)
    {
        write!(
            f,
            " prim={:?} created_at={} by {}",
            d.kind,
            d.created_at,
            d.created_on.as_deref().unwrap_or("<unnamed>"),
        )?;
    }
    if let Some((task_id, loc)) = wake.task() {
        write!(f, " task={task_id} spawned_at={loc}")?;
    }
    Ok(())
}

/// Whether a deadline-less waiter is PINNING quiescence right now — the only
/// case where its parking stack is a diagnosis rather than noise (a flash test
/// parks hundreds of these legitimately). One test per waiter class:
///
/// - an ASYNC waiter whose task still holds an `active_async` slot: the clock
///   may not advance past a counted task, and nothing but a signal for this
///   waiter can release it;
/// - a SYNC waiter parked from inside an async poll (its thread is `bridged`):
///   while it waits, every task that thread owes a poll is stranded.
fn pins_quiescence(entry: &Entry, reg: &Registry) -> bool {
    if let Some((task_id, _)) = entry.wake.task() {
        return reg.active_async_holders.contains_key(&task_id);
    }
    entry
        .parked
        .as_ref()
        .is_some_and(|p| reg.bridged.contains(&p.thread))
}

/// Diagnostic snapshot of the engine for hang dumps: counters, who pins
/// quiescence, every parked waiter (with the async primitive it is parked on and
/// the task waiting), plus every recorded engine primitive. Uses `try_lock` so a
/// dump from a panic/abort path can never itself hang on a held `core` lock.
impl fmt::Display for FlashInner {
    /// Diagnostic dump: marks each parked entry that currently pins the quiescence clock, and
    /// backtraces only print for a pinning waiter when `KITHARA_FLASH_SYNC_BT` was set.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let now = self.clock.now_nanos();
        let Ok(s) = self.core.try_lock() else {
            return write!(
                f,
                "virtual_now_ns={now}; engine core lock held — engine mid-operation"
            );
        };
        writeln!(
            f,
            "virtual_now_ns={now} active={} active_async={} real_io={} pace_anchor={} \
             yielders={} {}",
            s.registry.active,
            s.registry.active_async,
            s.sched.real_io,
            if s.sched.pace_anchor.is_some() {
                "set"
            } else {
                "none"
            },
            s.sched.yielders.len(),
            s.sched.advance_counts,
        )?;
        for (id, loc) in &s.registry.active_async_holders {
            write!(f, "  active_async holder task={id} spawned_at={loc}")?;
            if let Some(diag) = s.registry.task_diag.get(id) {
                write!(f, " state={:?} polls={}", diag.state.load(), diag.polls())?;
                if let Some(driver) = diag.driver() {
                    write!(f, " driver={driver:?}")?;
                }
            }
            writeln!(f)?;
        }
        for (key, holder) in &s.registry.active_sync_holders {
            writeln!(
                f,
                "  active holder thread={key:?} name={} held_for_real_ns={} resumed_from={}",
                holder.name.as_deref().unwrap_or("<unnamed>"),
                self.clock
                    .real_now_nanos()
                    .saturating_sub(holder.resumed_at_real_ns),
                holder.resumed_from,
            )?;
        }
        for ((deadline, id), entry) in &s.sched.timed {
            write!(
                f,
                "  timed id={id:?} kind={:?} deadline_in_ns={}",
                entry.kind,
                deadline.saturating_sub(now),
            )?;
            write_waiter_detail(f, &entry.kind, &entry.wake, &s.registry)?;
            writeln!(f)?;
        }
        for (id, entry) in &s.sched.indef {
            write!(f, "  indef id={id:?} kind={:?}", entry.kind)?;
            write_waiter_detail(f, &entry.kind, &entry.wake, &s.registry)?;
            if let Some(parked) = entry
                .parked
                .as_ref()
                .filter(|_| entry.wake.task().is_none())
            {
                write!(f, " thread={:?}", parked.thread)?;
            }
            let pins = pins_quiescence(entry, &s.registry);
            if pins {
                write!(f, " pins_clock")?;
            }
            writeln!(f)?;
            if let Some(bt) = entry.parked.as_ref().and_then(|p| p.stack.as_ref())
                && pins
            {
                writeln!(f, "    parked at:\n{bt}")?;
            }
        }
        if !s.registry.cv_desc.is_empty() {
            writeln!(f, "  engine primitives ({}):", s.registry.cv_desc.len())?;
            for (cvid, d) in &s.registry.cv_desc {
                writeln!(
                    f,
                    "    cvid={cvid} {:?} created_at={} by {}",
                    d.kind,
                    d.created_at,
                    d.created_on.as_deref().unwrap_or("<unnamed>"),
                )?;
            }
        }
        if !s.registry.bridged.is_empty() {
            writeln!(f, "  bridged={:?}", s.registry.bridged)?;
        }
        if !s.sched.unpark_pending.is_empty() {
            writeln!(f, "  unpark_pending={:?}", s.sched.unpark_pending)?;
        }
        if !s.sched.notify_permits.is_empty() {
            writeln!(f, "  notify_permits={:?}", s.sched.notify_permits)?;
        }
        Ok(())
    }
}
