#[cfg(not(target_arch = "wasm32"))]
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};

use crate::time::Instant;

/// Create a new unbounded channel.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (Sender(tx), Receiver(rx))
}

#[cfg(not(target_arch = "wasm32"))]
pub struct Sender<T>(std::sync::mpsc::Sender<T>);

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct Receiver<T>(std::sync::mpsc::Receiver<T>);

#[cfg(not(target_arch = "wasm32"))]
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

    #[kithara::test]
    fn recv_sync_timeout_returns_delivered_value_before_deadline() {
        let (tx, rx) = channel::<u32>();
        tx.send_sync(7).expect("send to live receiver");
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(rx.recv_sync_timeout(deadline), Ok(7));
    }

    #[kithara::test]
    fn recv_sync_timeout_times_out_when_no_value_arrives() {
        let (_tx, rx) = channel::<u32>();
        let deadline = Instant::now() + Duration::from_millis(10);
        assert_eq!(
            rx.recv_sync_timeout(deadline),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[kithara::test]
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
