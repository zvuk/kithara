use std::{future::Future, marker::PhantomData, panic::Location};

pub type Watched<F> = F;
pub type PermitPoll<F> = F;

#[must_use]
pub struct Permit {
    _not_send: PhantomData<*mut ()>,
}

#[must_use]
pub struct Pause {
    _not_send: PhantomData<*mut ()>,
}

#[inline]
pub fn permit() -> Permit {
    Permit {
        _not_send: PhantomData,
    }
}

#[doc(hidden)]
#[inline]
pub fn pause() -> Pause {
    Pause {
        _not_send: PhantomData,
    }
}

#[inline(always)]
#[track_caller]
pub(crate) fn forbid(_what: &'static str) {}

#[doc(hidden)]
#[inline(always)]
pub fn forbid_bridged(_spawn_loc: Option<&'static Location<'static>>) {}

#[doc(hidden)]
#[inline]
#[must_use]
pub fn permit_poll<F: Future>(fut: F) -> PermitPoll<F> {
    fut
}

#[inline]
#[must_use]
#[track_caller]
pub fn watch_blanket<F: Future>(_name: &'static str, fut: F) -> Watched<F> {
    fut
}

#[doc(hidden)]
#[inline]
#[must_use]
#[track_caller]
pub fn watch_budget<F: Future>(_name: &'static str, _budget_ms: u64, fut: F) -> Watched<F> {
    fut
}
