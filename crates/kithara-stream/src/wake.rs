#![forbid(unsafe_code)]

use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use kithara_platform::sync::Arc;

/// Reader-to-peer wake armed lock-free on the core and flushed by the off-core scheduler shell.
/// Delivering the peer task waker can issue a syscall, so the real-time core must not call it.
/// Off-core callers use [`notify_now`](Self::notify_now) directly, avoiding a seek delayed until
/// another worker pass. The caller chooses the path from its own statically known context.
#[derive(Default)]
pub struct DeferredWake {
    pending: AtomicBool,
    target: OnceLock<Arc<dyn WorkerWake>>,
}

impl DeferredWake {
    /// Bind the owning peer's off-core wake target once, before the reader becomes active.
    pub fn set_target(&self, target: Arc<dyn WorkerWake>) {
        let _ = self.target.set(target);
    }

    /// Record a pending wake without touching the runtime. Lock-free and
    /// syscall-free — safe to call from the forbid-blocking produce core.
    /// Repeated arms before a [`flush`](Self::flush) coalesce into one.
    pub fn arm(&self) {
        self.pending.store(true, Ordering::Release);
    }

    /// Deliver a pending wake, if any, and report whether one fired. Called from
    /// the scheduler shell off the produce core, so task wake syscalls
    /// stay on the unchecked path. A no-op when
    /// nothing was armed.
    pub fn flush(&self) -> bool {
        let armed = self.pending.swap(false, Ordering::AcqRel);
        if armed {
            self.notify_now();
        }
        armed
    }

    /// Deliver immediately from an off-core caller, without waiting for a scheduler pass.
    pub fn notify_now(&self) {
        if let Some(target) = self.target.get() {
            target.wake();
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
    use kithara_platform::sync::atomic::AtomicUsize;
    use kithara_test_utils::kithara;

    use super::*;

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl WorkerWake for WakeCounter {
        fn defer(&self) {
            panic!("a flushed wake must deliver off-core");
        }

        fn wake(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[kithara::test(miri)]
    fn flush_only_delivers_when_armed() {
        let wake = DeferredWake::default();
        let target = Arc::new(WakeCounter::default());
        wake.set_target(target.clone());
        assert!(!wake.flush(), "nothing armed: flush is a no-op");
        wake.arm();
        wake.arm();
        assert_eq!(target.0.load(Ordering::SeqCst), 0);
        assert!(wake.flush(), "armed: flush delivers once (arms coalesce)");
        assert_eq!(target.0.load(Ordering::SeqCst), 1);
        assert!(!wake.flush(), "pending cleared: no repeat delivery");
    }

    #[kithara::test(miri)]
    fn immediate_delivery_keeps_an_armed_wake_pending() {
        let wake = DeferredWake::default();
        let target = Arc::new(WakeCounter::default());
        wake.set_target(target.clone());
        wake.arm();
        wake.notify_now();
        assert_eq!(target.0.load(Ordering::SeqCst), 1);
        assert!(wake.flush());
        assert_eq!(target.0.load(Ordering::SeqCst), 2);
        assert!(!wake.flush());
    }
}
