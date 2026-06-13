use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub use crate::native::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};
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
        queue: Mutex::default(),
        cv: Condvar::default(),
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
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        if !self.0.receiver_alive.load(Ordering::Acquire) {
            return Err(SendError(value));
        }
        self.0.queue.lock().push_back(value);
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
            let guard = self.0.queue.lock();
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
    pub fn recv(&self) -> Result<T, RecvError> {
        let mut q = self.0.queue.lock();
        loop {
            if let Some(v) = q.pop_front() {
                return Ok(v);
            }
            if self.0.senders.load(Ordering::Acquire) == 0 {
                return Err(RecvError);
            }
            q = self.0.cv.wait(q);
        }
    }

    /// Try to receive without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError`] if no value is available or senders are dropped.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut q = self.0.queue.lock();
        match q.pop_front() {
            Some(v) => Ok(v),
            None if self.0.senders.load(Ordering::Acquire) == 0 => Err(TryRecvError::Disconnected),
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
    pub fn recv_timeout(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        let mut q = self.0.queue.lock();
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
            q = self.0.cv.wait_timeout(q, deadline);
        }
    }

    /// Iterate over received values, blocking until all senders disconnect.
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        std::iter::from_fn(move || self.recv().ok())
    }

    /// Iterate over currently-available values without blocking.
    pub fn try_iter(&self) -> impl Iterator<Item = T> + '_ {
        std::iter::from_fn(move || self.try_recv().ok())
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.0.receiver_alive.store(false, Ordering::Release);
    }
}
