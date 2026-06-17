#![forbid(unsafe_code)]

use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

use kithara_platform::{
    maybe_send::{MaybeSend, MaybeSync},
    sync::Notify,
};

/// Reader→peer wake split into an off-core *arm* and an off-core *flush*, so a
/// cross-thread `tokio::Notify` wake never fires on the real-time audio produce
/// core.
///
/// The produce core (the `#[rtsan_forbid_blocking]` region) must not call
/// `Notify::notify_one`: scheduling the downloader's parked task cross-thread is
/// a `kevent` syscall on macOS, illegal on the RT path. A reader-blocked probe
/// or a seek-epoch bump reached on the core therefore [`arm`](Self::arm)s a
/// lock-free flag; the audio scheduler shell [`flush`](Self::flush)es it once
/// per pass, off the forbid path, where the `notify_one` is allowed.
///
/// Off the core (an off-worker `Stream::seek` priming a range, the ABR
/// controller) the caller [`notify_now`](Self::notify_now)s directly — no defer,
/// so a synchronous seek is never stalled waiting for the worker's next pass.
///
/// The caller picks `arm` vs `notify_now` from its own statically-known context;
/// this type holds no global state and makes no context decision itself.
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

    /// Future the peer's waker-forwarding task awaits. Resolves on the next
    /// [`flush`](Self::flush) / [`notify_now`](Self::notify_now); tokio's
    /// stored-permit semantics mean a wake delivered between awaits is not lost.
    pub fn notified(&self) -> impl Future<Output = ()> + '_ {
        self.notify.notified()
    }

    /// Immediate wake for off-core callers (an off-worker seek prime, the ABR
    /// controller) — never reached from the RT produce core.
    pub fn notify_now(&self) {
        self.notify.notify_one();
    }
}

/// Data-arrival → audio-worker wake. The inverse direction of
/// [`DeferredWake`]: where `DeferredWake` is the reader→peer nudge (the audio
/// produce core asking the downloader for more), this is the producer→worker
/// nudge — the downloader, having just written/committed segment bytes,
/// re-ticks an underran audio worker the instant its data lands instead of
/// leaving it to rediscover the bytes on its next wall-clock poll.
///
/// The implementor (the audio worker's scheduler wake) MUST make [`wake`] a
/// wait-free, syscall-bounded signal (an atomic bump + `thread::unpark`), since
/// it is called cross-thread from the downloader's write/settle path. It is
/// NOT called from the real-time produce core, so it carries no
/// forbid-blocking constraint — but it must not block the downloader either.
///
/// Segmented sources (HLS) hold an optional handle and fire it from their
/// off-RT write/commit sites only; non-segmented sources never set one.
pub trait WorkerWake: MaybeSend + MaybeSync {
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
