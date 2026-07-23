use std::sync::atomic::{AtomicBool, Ordering};

use kithara_platform::{
    sync::CondvarGate,
    time::{Duration, Instant},
};

pub(super) struct ReadinessGate {
    failed: AtomicBool,
    gate: CondvarGate<bool>,
}

impl ReadinessGate {
    pub(super) fn new(initial: bool) -> Self {
        Self {
            gate: CondvarGate::new(initial),
            failed: AtomicBool::new(false),
        }
    }

    pub(super) fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.gate.notify_all();
    }

    pub(super) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub(super) fn is_ready(&self) -> bool {
        *self.gate.lock()
    }

    pub(super) fn mark_ready(&self) {
        *self.gate.lock() = true;
        self.gate.notify_all();
    }

    pub(super) fn wait_until_ready(&self, should_abort: &dyn Fn() -> bool) -> bool {
        const COND_WAIT_MS: u64 = 100;

        loop {
            if self.is_failed() {
                return false;
            }
            let ready = {
                let guard = self.gate.lock();
                if *guard {
                    return !self.is_failed();
                }
                let deadline = Instant::now() + Duration::from_millis(COND_WAIT_MS);
                let next = self.gate.wait_until(guard, deadline);
                *next
            };
            if ready {
                return !self.is_failed();
            }
            if self.is_failed() || should_abort() {
                return false;
            }
        }
    }
}
