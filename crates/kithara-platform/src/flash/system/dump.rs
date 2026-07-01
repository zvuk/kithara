use std::fmt;

use super::{FlashInner, Registry, sched::WaitKind, wake::Wake};

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

/// Diagnostic snapshot of the engine for hang dumps: counters, who pins
/// quiescence, every parked waiter (with the async primitive it is parked on and
/// the task waiting), plus every recorded engine primitive. Uses `try_lock` so a
/// dump from a panic/abort path can never itself hang on a held `core` lock.
impl fmt::Display for FlashInner {
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
            "virtual_now_ns={now} active={} active_async={} real_io={} pace_anchor={} yielders={}",
            s.registry.active,
            s.registry.active_async,
            s.sched.real_io,
            if s.sched.pace_anchor.is_some() {
                "set"
            } else {
                "none"
            },
            s.sched.yielders.len(),
        )?;
        // Name WHO pins quiescence instead of leaving bare counters. Async: the
        // spawn site of every task in a non-quiescent gate state (a task mid
        // bridged-wait has released its slot, so `active_async` may be < the
        // listed count — those are not pinning). Sync: every dedicated pacer
        // currently `Running` (`active` may exceed the list by a reserved-but-
        // unclaimed slot or an in-flight wake bump, which carry no thread yet).
        for (id, loc) in &s.registry.active_async_holders {
            writeln!(f, "  active_async holder task={id} spawned_at={loc}")?;
        }
        for (key, holder) in &s.registry.active_sync_holders {
            writeln!(
                f,
                "  active holder thread={key:?} name={}",
                holder.name.as_deref().unwrap_or("<unnamed>"),
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
            writeln!(f)?;
        }
        // Every engine-backed async primitive that recorded its provenance
        // (`describe_cvid`, under `KITHARA_FLASH_SYNC_TRACE`), so async primitives
        // land in the dump even when no waiter is currently parked on them.
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
        if !s.sched.unpark_pending.is_empty() {
            writeln!(f, "  unpark_pending={:?}", s.sched.unpark_pending)?;
        }
        if !s.sched.notify_permits.is_empty() {
            writeln!(f, "  notify_permits={:?}", s.sched.notify_permits)?;
        }
        Ok(())
    }
}
