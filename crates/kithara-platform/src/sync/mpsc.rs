//! Synchronous MPSC channel.
//!
//! * **Native, non-sim** — thin wrapper over [`std::sync::mpsc`].
//! * **Native, `flash`** — backed by the platform [`Mutex`] + [`Condvar`], so
//!   a blocking `recv_sync` parks on the quiescence engine (sim-accounted: it
//!   drops the caller's `active` credit) and `send_sync` wakes it via the
//!   condvar. A raw `std::sync::mpsc::recv()` blocks on a real OS condvar that
//!   the engine cannot see — the caller keeps its `active` slot, so the virtual
//!   clock freezes (or a `send` to a peer parked in `park_timeout` never lands,
//!   because the channel wake and the engine park are different mechanisms).
//! * **WASM** — backed by `wasm_safe_thread::mpsc`.

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
use crate::time::Instant;

/// Create a new unbounded channel.
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (Sender(tx), Receiver(rx))
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub struct Sender<T>(std::sync::mpsc::Sender<T>);

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
impl<T> Sender<T> {
    /// Send a value synchronously.
    ///
    /// # Errors
    ///
    /// Returns [`SendError`] if the receiver has been dropped.
    pub fn send_sync(&self, value: T) -> Result<(), SendError<T>> {
        self.0.send(value)
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub struct Receiver<T>(std::sync::mpsc::Receiver<T>);

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
impl<T> Receiver<T> {
    /// Block until a value arrives.
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    pub fn recv_sync(&self) -> Result<T, RecvError> {
        self.0.recv()
    }

    /// Try to receive without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError`] if no value is available or senders are dropped.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.0.try_recv()
    }

    /// Block until a value arrives or `deadline` elapses.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] when no value arrives before
    /// `deadline`, or [`RecvTimeoutError::Disconnected`] if all senders are
    /// dropped.
    pub fn recv_sync_timeout(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        let now = Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        self.0.recv_timeout(remaining)
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use sim_chan::{
    Receiver, RecvError, RecvTimeoutError, SendError, Sender, TryRecvError, channel,
};

/// Sim-time MPSC: a condvar-backed queue. Both the [`Mutex`](crate::Mutex) and
/// the [`Condvar`](crate::sync::Condvar) it stands on are already engine-aware
/// under `flash`, so `recv_sync` parks on the quiescence engine (its `active`
/// credit drops while it waits) and `send_sync`/sender-drop wake it through the
/// condvar — composing with `park_timeout`/`Instant` on the single virtual clock.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
mod sim_chan {
    pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};
    use std::{
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use crate::{Mutex, sync::Condvar, time::Instant};

    struct Chan<T> {
        queue: Mutex<VecDeque<T>>,
        cv: Condvar,
        /// Live `Sender` count. Reaching 0 means "disconnected": a blocked
        /// `recv` returns [`RecvError`], `try_recv` returns `Disconnected`.
        senders: AtomicUsize,
        /// Cleared on `Receiver` drop so a later `send` reports `SendError`.
        receiver_alive: AtomicBool,
    }

    pub struct Sender<T>(Arc<Chan<T>>);
    pub struct Receiver<T>(Arc<Chan<T>>);

    /// Create a new unbounded channel.
    #[must_use]
    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        let chan = Arc::new(Chan {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            senders: AtomicUsize::new(1),
            receiver_alive: AtomicBool::new(true),
        });
        (Sender(Arc::clone(&chan)), Receiver(chan))
    }

    impl<T> Sender<T> {
        /// Send a value synchronously.
        ///
        /// # Errors
        ///
        /// Returns [`SendError`] if the receiver has been dropped.
        pub fn send_sync(&self, value: T) -> Result<(), SendError<T>> {
            if !self.0.receiver_alive.load(Ordering::Acquire) {
                return Err(SendError(value));
            }
            self.0.queue.lock_sync().push_back(value);
            // A receiver registers its condvar waiter UNDER the queue lock and
            // releases the lock only as it parks, so this notify (which we issue
            // after releasing the lock above) can never land before the waiter
            // is registered — no lost wakeup.
            self.0.cv.notify_one();
            Ok(())
        }
    }

    impl<T> Clone for Sender<T> {
        fn clone(&self) -> Self {
            self.0.senders.fetch_add(1, Ordering::AcqRel);
            Self(Arc::clone(&self.0))
        }
    }

    impl<T> Drop for Sender<T> {
        fn drop(&mut self) {
            if self.0.senders.fetch_sub(1, Ordering::AcqRel) == 1 {
                // Last sender gone. Take the queue lock so this serializes with a
                // receiver registering its waiter under the same lock, then wake
                // it to observe the disconnect (no lost wakeup against the
                // unlocked `senders` predicate).
                let guard = self.0.queue.lock_sync();
                self.0.cv.notify_all();
                drop(guard);
            }
        }
    }

    impl<T> Receiver<T> {
        /// Block until a value arrives.
        ///
        /// # Errors
        ///
        /// Returns [`RecvError`] if all senders have been dropped.
        pub fn recv_sync(&self) -> Result<T, RecvError> {
            let mut q = self.0.queue.lock_sync();
            loop {
                if let Some(v) = q.pop_front() {
                    return Ok(v);
                }
                if self.0.senders.load(Ordering::Acquire) == 0 {
                    return Err(RecvError);
                }
                q = self.0.cv.wait_sync(q);
            }
        }

        /// Try to receive without blocking.
        ///
        /// # Errors
        ///
        /// Returns [`TryRecvError`] if no value is available or senders are dropped.
        pub fn try_recv(&self) -> Result<T, TryRecvError> {
            let mut q = self.0.queue.lock_sync();
            match q.pop_front() {
                Some(v) => Ok(v),
                None if self.0.senders.load(Ordering::Acquire) == 0 => {
                    Err(TryRecvError::Disconnected)
                }
                None => Err(TryRecvError::Empty),
            }
        }

        /// Block until a value arrives or `deadline` elapses.
        ///
        /// # Errors
        ///
        /// Returns [`RecvTimeoutError::Timeout`] when no value arrives before
        /// `deadline`, or [`RecvTimeoutError::Disconnected`] if all senders are
        /// dropped.
        pub fn recv_sync_timeout(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
            let mut q = self.0.queue.lock_sync();
            loop {
                if let Some(v) = q.pop_front() {
                    return Ok(v);
                }
                if self.0.senders.load(Ordering::Acquire) == 0 {
                    return Err(RecvTimeoutError::Disconnected);
                }
                if Instant::now() >= deadline {
                    return Err(RecvTimeoutError::Timeout);
                }
                q = self.0.cv.wait_sync_timeout(q, deadline);
            }
        }
    }

    impl<T> Drop for Receiver<T> {
        fn drop(&mut self) {
            self.0.receiver_alive.store(false, Ordering::Release);
        }
    }
}

#[cfg(target_arch = "wasm32")]
use wasm_safe_thread::mpsc as wasm_mpsc;
#[cfg(target_arch = "wasm32")]
pub use wasm_safe_thread::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};

/// Create a new unbounded channel.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = wasm_mpsc::channel();
    (Sender(tx), Receiver(rx))
}

#[cfg(target_arch = "wasm32")]
pub struct Sender<T>(wasm_safe_thread::mpsc::Sender<T>);

#[cfg(target_arch = "wasm32")]
impl<T> Sender<T> {
    /// Send a value synchronously.
    ///
    /// # Errors
    ///
    /// Returns [`SendError`] if the receiver has been dropped.
    pub fn send_sync(&self, value: T) -> Result<(), SendError<T>> {
        self.0.send_sync(value)
    }
}

#[cfg(target_arch = "wasm32")]
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(target_arch = "wasm32")]
pub struct Receiver<T>(wasm_safe_thread::mpsc::Receiver<T>);

#[cfg(target_arch = "wasm32")]
impl<T> Receiver<T> {
    /// Await a value asynchronously (WASM only).
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    pub async fn recv_async(&self) -> Result<T, RecvError> {
        self.0.recv_async().await
    }

    /// Block until a value arrives.
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    pub fn recv_sync(&self) -> Result<T, RecvError> {
        self.0.recv_sync()
    }

    /// Try to receive without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError`] if no value is available or senders are dropped.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.0.try_recv()
    }

    /// Block until a value arrives or `deadline` elapses.
    ///
    /// On worker threads this parks via `Atomics.wait`; on the browser main
    /// thread (where `Atomics.wait` is disallowed) it falls back to spinning
    /// until the deadline. Callers must only block on worker threads.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] when no value arrives before
    /// `deadline`, or [`RecvTimeoutError::Disconnected`] if all senders are
    /// dropped.
    pub fn recv_sync_timeout(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        self.0.recv_sync_timeout(deadline)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::time::Instant;

    // `flash(false)`: these unit tests build a `crate::time::Instant` deadline and
    // pass it to the crate's own `recv_sync_timeout`. The lexical flash rewrite
    // would retarget `Instant::now()` onto `::kithara_platform::time::flash_virtual_now`,
    // whose `Instant` resolves through the dev-dep copy of `kithara_platform`
    // (`kithara-test-utils` pulls a non-flash copy) — a different `Instant` type
    // than the crate-local one the method expects. They do not test flash time
    // behaviour, so opting out of the rewrite keeps a single, crate-local `Instant`.
    #[kithara::test(flash(false))]
    fn recv_sync_timeout_returns_delivered_value_before_deadline() {
        let (tx, rx) = channel::<u32>();
        tx.send_sync(7).expect("send to live receiver");
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(rx.recv_sync_timeout(deadline), Ok(7));
    }

    #[kithara::test(flash(false))]
    fn recv_sync_timeout_times_out_when_no_value_arrives() {
        let (_tx, rx) = channel::<u32>();
        let deadline = Instant::now() + Duration::from_millis(10);
        assert_eq!(
            rx.recv_sync_timeout(deadline),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[kithara::test(flash(false))]
    fn recv_sync_timeout_reports_disconnect_when_senders_dropped() {
        let (tx, rx) = channel::<u32>();
        drop(tx);
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            rx.recv_sync_timeout(deadline),
            Err(RecvTimeoutError::Disconnected)
        );
    }
}
