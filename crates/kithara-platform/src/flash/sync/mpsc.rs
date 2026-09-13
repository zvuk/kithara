pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::{
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};

struct Chan<T> {
    /// Cleared on `Receiver` drop so a later `send` reports `SendError`.
    receiver_alive: AtomicBool,
    /// Live `Sender` count. Reaching 0 means "disconnected": a blocked
    /// `recv` returns [`RecvError`], `try_recv` returns `Disconnected`.
    senders: AtomicUsize,
    cv: Condvar,
    queue: Mutex<VecDeque<T>>,
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
        let mut queue = self.0.queue.lock();
        if !self.0.receiver_alive.load(Ordering::Acquire) {
            return Err(SendError(value));
        }
        queue.push_back(value);
        drop(queue);
        // WHY: A receiver registers its condvar waiter UNDER the queue lock and releases the lock only as it parks, so this notify (which we
        // issue after releasing the lock above) can never land before the waiter is registered - no lost wakeup.
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
            // WHY: Last sender gone. Take the queue lock so this serializes with a receiver registering its waiter under the same lock, then
            // wake it to observe the disconnect (no lost wakeup against the unlocked `senders` predicate).
            let guard = self.0.queue.lock();
            self.0.cv.notify_all();
            drop(guard);
        }
    }
}

impl<T> Receiver<T> {
    /// Block until a value arrives.
    ///
    /// The `no_block` detector hears this at the call and not at the park: a
    /// message already queued returns without ever waiting, and a detector
    /// that only saw the wait would judge the same code differently from one
    /// run to the next.
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    #[track_caller]
    pub fn recv(&self) -> Result<T, RecvError> {
        crate::no_block::forbid("mpsc::recv");
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

    /// Block until a value arrives or `deadline` elapses.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] when no value arrives before
    /// `deadline`, or [`RecvTimeoutError::Disconnected`] if all senders are
    /// dropped.
    ///
    /// Reaches the `no_block` detector at the call, like [`Self::recv`].
    #[track_caller]
    pub fn recv_timeout(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        crate::no_block::forbid("mpsc::recv_timeout");
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

    delegate::delegate! {
        to self {
            /// Iterate over received values, blocking until all senders disconnect.
            #[expr(std::iter::from_fn(move || $.ok()))]
            #[call(recv)]
            pub fn iter(&self) -> impl Iterator<Item = T> + '_;
            /// Iterate over currently-available values without blocking.
            #[expr(std::iter::from_fn(move || $.ok()))]
            #[call(try_recv)]
            pub fn try_iter(&self) -> impl Iterator<Item = T> + '_;
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut queue = self.0.queue.lock();
        self.0.receiver_alive.store(false, Ordering::Release);
        let queued = std::mem::take(&mut *queue);
        drop(queue);
        drop(queued);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{TryRecvError, channel};

    #[kithara::test]
    fn dropping_receiver_drops_queued_messages() {
        let (outer_tx, outer_rx) = channel();
        let (reply_tx, reply_rx) = channel::<()>();

        assert!(outer_tx.send(reply_tx).is_ok());
        drop(outer_rx);

        assert!(matches!(
            reply_rx.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }
}
