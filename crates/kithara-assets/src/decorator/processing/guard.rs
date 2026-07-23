use kithara_platform::sync::Arc;

use super::gate::ReadinessGate;

pub(super) struct GateGuard {
    readiness: Arc<ReadinessGate>,
    armed: bool,
}

impl GateGuard {
    pub(super) fn new(readiness: Arc<ReadinessGate>) -> Self {
        Self {
            readiness,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(super) fn fail(&self) {
        self.readiness.fail();
    }

    pub(super) fn is_ready(&self) -> bool {
        self.readiness.is_ready()
    }

    pub(super) fn mark_ready(&self) {
        self.readiness.mark_ready();
    }

    pub(super) fn shared(&self) -> Arc<ReadinessGate> {
        Arc::clone(&self.readiness)
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        if self.armed {
            self.readiness.fail();
        }
    }
}
