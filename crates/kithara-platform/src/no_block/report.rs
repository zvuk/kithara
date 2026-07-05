use std::{panic::Location, time::Duration};

use super::mode::{Mode, mode};

const SPIN_CPU_FRACTION: f64 = 0.8;

pub(super) fn forbidden(
    what: &'static str,
    task: &'static str,
    spawned: &'static Location<'static>,
    at: &'static Location<'static>,
) {
    match mode() {
        Mode::Off => {}
        Mode::Census => eprintln!(
            "[no_block][census] blocking {what} inside async poll of `{task}` \
             (spawned at {spawned}) at {at}"
        ),
        Mode::Panic => panic!(
            "[no_block] blocking {what} inside async poll of `{task}` \
             (spawned at {spawned})\n  at {at}\n  sanctioned bridge? mark the fn \
             with #[kithara::allow_block]"
        ),
    }
}

pub(super) fn over_budget(
    task: &'static str,
    spawned: &'static Location<'static>,
    wall: Duration,
    cpu: Option<Duration>,
    budget: Duration,
) {
    let kind = match cpu {
        Some(c) if c.as_secs_f64() >= wall.as_secs_f64() * SPIN_CPU_FRACTION => "CPU spin",
        Some(_) => "blocked wait (lock/sleep/IO)",
        None => "unclassified (no thread CPU clock)",
    };
    match mode() {
        Mode::Off => {}
        Mode::Census => eprintln!(
            "[no_block][census] task `{task}` (spawned at {spawned}): single poll took \
             {wall:?} (cpu {cpu:?}, budget {budget:?}) - {kind}"
        ),
        Mode::Panic => panic!(
            "[no_block] task `{task}` (spawned at {spawned}): single poll took {wall:?} \
             (cpu {cpu:?}, budget {budget:?}) - {kind}\n  sanctioned blocking? mark \
             the blocking fn with #[kithara::allow_block]"
        ),
    }
}
