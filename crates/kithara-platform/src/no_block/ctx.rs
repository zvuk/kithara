use std::{cell::Cell, marker::PhantomData, panic::Location, time::Instant};

use cpu_time::ThreadTime;

use super::clock;

pub(super) type TaskId = (&'static str, &'static Location<'static>);

struct NbCtx {
    cur: Cell<Option<TaskId>>,
    paused_cpu_nanos: Cell<u128>,
    paused_nanos: Cell<u128>,
    permit_depth: Cell<u32>,
}

thread_local! {
    static CTX: NbCtx = const {
        NbCtx {
            cur: Cell::new(None),
            permit_depth: Cell::new(0),
            paused_cpu_nanos: Cell::new(0),
            paused_nanos: Cell::new(0),
        }
    };
}

pub(super) fn in_poll() -> Option<TaskId> {
    CTX.with(|c| c.cur.get())
}

pub(super) fn permitted() -> bool {
    CTX.with(|c| c.permit_depth.get() > 0)
}

pub(super) fn paused_nanos() -> u128 {
    CTX.with(|c| c.paused_nanos.get())
}

/// Thread CPU spent inside sanctioned regions, the twin of [`paused_nanos`].
///
/// A pause removes its region from the poll's wall, so the CPU that region
/// burned has to leave with it: a budget that subtracts one and not the other
/// weighs a net wall against a gross CPU, and every sanctioned pass of real
/// arithmetic then reads as a spin.
pub(super) fn paused_cpu_nanos() -> u128 {
    CTX.with(|c| c.paused_cpu_nanos.get())
}

pub(super) struct PollScope {
    prev: Option<TaskId>,
}

impl PollScope {
    pub(super) fn enter(id: TaskId) -> Self {
        Self {
            prev: CTX.with(|c| c.cur.replace(Some(id))),
        }
    }
}

impl Drop for PollScope {
    fn drop(&mut self) {
        CTX.with(|c| c.cur.set(self.prev));
    }
}

#[must_use]
pub struct Pause {
    cpu_start: Option<ThreadTime>,
    start: Instant,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for Pause {
    fn drop(&mut self) {
        let add = self.start.elapsed().as_nanos();
        let cpu = self
            .cpu_start
            .and_then(|start| start.try_elapsed().ok())
            .map_or(0, |cpu| cpu.as_nanos());
        CTX.with(|c| {
            c.paused_nanos.set(c.paused_nanos.get().saturating_add(add));
            c.paused_cpu_nanos
                .set(c.paused_cpu_nanos.get().saturating_add(cpu));
        });
    }
}

pub(super) fn pause_now() -> Pause {
    let start = Instant::now();
    Pause {
        _not_send: PhantomData,
        cpu_start: clock::snapshot(start),
        start,
    }
}

#[must_use]
pub struct Permit {
    _pause: Pause,
    _not_send: PhantomData<*mut ()>,
}

impl Permit {
    pub(super) fn enter() -> Self {
        CTX.with(|c| c.permit_depth.set(c.permit_depth.get() + 1));
        Self {
            _not_send: PhantomData,
            _pause: pause_now(),
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        CTX.with(|c| {
            c.permit_depth.set(c.permit_depth.get().saturating_sub(1));
        });
    }
}
