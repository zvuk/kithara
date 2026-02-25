//! Cross-platform wrappers over `kanal`.
//!
//! The API is intentionally small and sync-only:
//! - [`bounded`], [`unbounded`]
//! - [`Sender`], [`Receiver`]
//!
//! All operations use kanal's *realtime* variants (`try_lock` only),
//! so the internal `RawMutexLock` never escalates to `std::thread::sleep`
//! or `std::thread::park`.  This is critical on wasm32+atomics where
//! those calls compile to `memory.atomic.wait32` (`Atomics.wait`),
//! which is illegal on the browser main thread.

pub use kanal::{ReceiveError, SendError};

/// Sender wrapper with cross-platform semantics.
#[derive(Debug)]
pub struct Sender<T>(kanal::Sender<T>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Sender<T> {
    /// Number of queued items currently visible to this channel endpoint.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the channel currently has no queued items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Send an item, spinning until the channel accepts it.
    ///
    /// Uses `try_send_option_realtime` internally so the kanal mutex
    /// is never held longer than a single `try_lock` CAS.
    ///
    /// # Errors
    /// Returns [`SendError`] if the receiver side is closed.
    pub fn send(&self, item: T) -> Result<(), SendError> {
        let mut pending = Some(item);
        loop {
            match self.0.try_send_option_realtime(&mut pending) {
                Ok(true) => return Ok(()),
                Ok(false) => spin_wait_hint(),
                Err(err) => return Err(err),
            }
        }
    }

    /// Try send without blocking.
    ///
    /// Returns `Ok(false)` when the channel is full **or** the internal
    /// lock is momentarily contended (realtime semantics).
    ///
    /// # Errors
    /// Returns [`SendError`] if the receiver side is closed.
    pub fn try_send(&self, item: T) -> Result<bool, SendError> {
        self.0.try_send_realtime(item)
    }

    /// Try send `Option<T>` without blocking.
    ///
    /// The value stays inside `Option` when the channel is full or
    /// the lock is contended, so the caller retains ownership.
    ///
    /// # Errors
    /// Returns [`SendError`] if the receiver side is closed.
    pub fn try_send_option(&self, item: &mut Option<T>) -> Result<bool, SendError> {
        self.0.try_send_option_realtime(item)
    }
}

/// Receiver wrapper with cross-platform semantics.
#[derive(Debug)]
pub struct Receiver<T>(kanal::Receiver<T>);

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Receiver<T> {
    /// Number of queued items currently visible to this channel endpoint.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the channel currently has no queued items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Receive one item, spinning until data is available.
    ///
    /// Uses `try_recv_realtime` internally so the kanal mutex
    /// is never held longer than a single `try_lock` CAS.
    ///
    /// # Errors
    /// Returns [`ReceiveError`] if the sender side is closed.
    pub fn recv(&self) -> Result<T, ReceiveError> {
        loop {
            match self.0.try_recv_realtime() {
                Ok(Some(item)) => return Ok(item),
                Ok(None) => spin_wait_hint(),
                Err(err) => return Err(err),
            }
        }
    }

    /// Try receive without blocking.
    ///
    /// Returns `Ok(None)` when the channel is empty **or** the internal
    /// lock is momentarily contended (realtime semantics).
    ///
    /// # Errors
    /// Returns [`ReceiveError`] if the sender side is closed.
    pub fn try_recv(&self) -> Result<Option<T>, ReceiveError> {
        self.0.try_recv_realtime()
    }
}

/// Create a bounded channel.
#[must_use]
pub fn bounded<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = kanal::bounded(capacity);
    (Sender(tx), Receiver(rx))
}

/// Create an unbounded channel.
#[must_use]
pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = kanal::unbounded();
    (Sender(tx), Receiver(rx))
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test]
    fn bounded_send_recv_roundtrip() {
        let (tx, rx) = bounded(1);
        tx.send(42).unwrap();
        let got = rx.recv().unwrap();
        assert_eq!(got, 42);
    }

    #[kithara::test]
    fn try_send_reports_full_channel() {
        let (tx, rx) = bounded(1);
        assert!(tx.try_send(1).unwrap());
        // Realtime try_send may return false for full OR contended lock.
        // With a bounded(1) channel and one item already in, this must
        // return false (channel full).
        assert!(!tx.try_send(2).unwrap());
        assert_eq!(rx.recv().unwrap(), 1);
    }

    #[kithara::test]
    fn unbounded_try_recv_empty_then_value() {
        let (tx, rx) = unbounded();
        assert!(matches!(rx.try_recv(), Ok(None)));
        tx.send(7).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Some(7))));
    }
}

#[inline]
fn spin_wait_hint() {
    #[cfg(target_arch = "wasm32")]
    {
        crate::thread::yield_now();
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        core::hint::spin_loop();
    }
}
