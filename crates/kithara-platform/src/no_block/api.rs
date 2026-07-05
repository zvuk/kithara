use super::{ctx, report};
use crate::no_block::{Pause, Permit};

pub fn permit() -> Permit {
    Permit::enter()
}

#[doc(hidden)]
pub fn pause() -> Pause {
    ctx::pause_now()
}

#[track_caller]
pub(crate) fn forbid(what: &'static str) {
    let Some((task, spawned)) = ctx::in_poll() else {
        return;
    };
    if ctx::permitted() {
        return;
    }
    report::forbidden(what, task, spawned, std::panic::Location::caller());
}
