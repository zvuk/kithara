#![forbid(unsafe_code)]

use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

use kithara_platform::sync::Notify;

/// Reader-to-peer wake armed lock-free on the core and flushed by the off-core scheduler shell.
/// `Notify::notify_one` can issue a macOS kevent syscall, so the real-time core must not call it.
/// Off-core callers use [`notify_now`](Self::notify_now) directly, avoiding a seek delayed until
/// another worker pass. The caller chooses the path from its own statically known context.
#[derive(Default)]
pub struct DeferredWake {
    pending: AtomicBool,
    notify: Notify,
}

impl DeferredWake {
    /// Record a pending wake without touching the runtime. Lock-free and
    /// syscall-free — safe to call from the forbid-blocking produce core.
    /// Repeated arms before a [`flush`](Self::flush) coalesce into one.
    pub fn arm(&self) {
        self.pending.store(true, Ordering::Release);
    }

    /// Deliver a pending wake, if any, and report whether one fired. Called from
    /// the scheduler shell off the produce core, so the cross-thread
    /// `notify_one` (a `kevent`) stays on the unchecked path. A no-op when
    /// nothing was armed.
    pub fn flush(&self) -> bool {
        let armed = self.pending.swap(false, Ordering::AcqRel);
        if armed {
            self.notify.notify_one();
        }
        armed
    }

    delegate::delegate! {
        to self.notify {
            /// Future the peer's waker-forwarding task awaits. Resolves on the next
            /// [`flush`](Self::flush) / [`notify_now`](Self::notify_now); tokio's
            /// stored-permit semantics mean a wake delivered between awaits is not lost.
            pub fn notified(&self) -> impl Future<Output = ()> + '_;
            /// Immediate wake for off-core callers (an off-worker seek prime, the ABR
            /// controller) — never reached from the RT produce core.
            #[call(notify_one)]
            pub fn notify_now(&self);
        }
    }
}

/// Off-RT data-arrival wake to re-tick an underran audio worker immediately, opposite [`DeferredWake`].
/// [`wake`](Self::wake) must be wait-free and syscall-bounded (atomic bump plus thread unpark),
/// never blocking the downloader. Sources fire optional handles only from off-RT write/commit sites.
pub trait WorkerWake: Send + Sync {
    /// Coalesce a future worker pass without unparking from the real-time path.
    fn defer(&self);

    /// Wake the audio worker so it re-ticks the decoder now that data landed.
    fn wake(&self);
}
#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn flush_only_delivers_when_armed() {
        let wake = DeferredWake::default();
        assert!(!wake.flush(), "nothing armed: flush is a no-op");
        wake.arm();
        wake.arm();
        assert!(wake.flush(), "armed: flush delivers once (arms coalesce)");
        assert!(!wake.flush(), "pending cleared: no repeat delivery");
    }
}
