use std::{
    hint::spin_loop,
    sync::atomic::{AtomicU8, Ordering},
};

/// Entry to a channel's ring, shared by its two halves: a plain send or scoped
/// publication enters before its ring-index store and leaves after it. The
/// inbox closes the gate before its last drain, which reads every admitted batch.
#[derive(Default)]
pub(super) struct Gate(AtomicU8);

impl Gate {
    /// The inbox closed the gate: no batch enters the ring again.
    const CLOSED: u8 = 1;
    /// A send is between entering the gate and leaving it.
    const SENDING: u8 = 2;

    pub(super) fn is_closed(&self) -> bool {
        self.0.load(Ordering::Acquire) & Self::CLOSED != 0
    }

    /// Enters for one send; `false` once the gate is closed.
    pub(super) fn enter(&self) -> bool {
        if self.0.fetch_or(Self::SENDING, Ordering::AcqRel) & Self::CLOSED == 0 {
            return true;
        }
        self.leave();
        false
    }

    /// Leaves after a send entered; its push is visible to the inbox that
    /// closes the gate next.
    pub(super) fn leave(&self) {
        self.0.fetch_and(!Self::SENDING, Ordering::Release);
    }

    /// Closes the gate, then waits out a send that entered before it closed:
    /// that send is one ring push, never a wait on the inbox.
    pub(super) fn close(&self) {
        self.0.fetch_or(Self::CLOSED, Ordering::AcqRel);
        while self.0.load(Ordering::Acquire) & Self::SENDING != 0 {
            spin_loop();
        }
    }
}
