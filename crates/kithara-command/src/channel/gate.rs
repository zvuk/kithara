use std::sync::atomic::{AtomicU8, Ordering};

/// Entry to a channel's ring. Closure refuses new sends without waiting for an
/// admitted sender; its owner recovers orphaned batches after the inbox releases
/// the ring's consumer and receipt producers.
#[derive(Default)]
pub(super) struct Gate(AtomicU8);

impl Gate {
    /// The inbox closed the gate: no batch enters the ring again.
    const CLOSED: u8 = 1;

    pub(super) fn is_closed(&self) -> bool {
        self.0.load(Ordering::Acquire) & Self::CLOSED != 0
    }

    /// Enters for one send; `false` once the gate is closed.
    pub(super) fn enter(&self) -> bool {
        !self.is_closed()
    }

    /// Refuses new sends without waiting on a potentially preempted producer.
    pub(super) fn close(&self) {
        self.0.fetch_or(Self::CLOSED, Ordering::AcqRel);
    }
}
