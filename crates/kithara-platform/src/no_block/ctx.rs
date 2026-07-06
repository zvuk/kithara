use std::{cell::Cell, marker::PhantomData, panic::Location, time::Instant};

pub(super) type TaskId = (&'static str, &'static Location<'static>);

struct NbCtx {
    cur: Cell<Option<TaskId>>,
    permit_depth: Cell<u32>,
    paused_nanos: Cell<u128>,
}

thread_local! {
    static CTX: NbCtx = const {
        NbCtx {
            cur: Cell::new(None),
            permit_depth: Cell::new(0),
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
    _not_send: PhantomData<*mut ()>,
    start: Instant,
}

impl Drop for Pause {
    fn drop(&mut self) {
        let add = self.start.elapsed().as_nanos();
        CTX.with(|c| c.paused_nanos.set(c.paused_nanos.get().saturating_add(add)));
    }
}

pub(super) fn pause_now() -> Pause {
    Pause {
        _not_send: PhantomData,
        start: Instant::now(),
    }
}

#[must_use]
pub struct Permit {
    _not_send: PhantomData<*mut ()>,
    _pause: Pause,
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
