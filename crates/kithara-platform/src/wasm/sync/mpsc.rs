use wasm_safe_thread::mpsc as wasm_mpsc;
pub use wasm_safe_thread::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};
use web_time::Instant;

/// Create a new unbounded channel.
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = wasm_mpsc::channel();
    (Sender(tx), Receiver(rx))
}

pub struct Sender<T>(wasm_safe_thread::mpsc::Sender<T>);

impl<T> Sender<T> {
    /// Send a value synchronously.
    ///
    /// # Errors
    ///
    /// Returns [`SendError`] if the receiver has been dropped.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.0.send_sync(value)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct Receiver<T>(wasm_safe_thread::mpsc::Receiver<T>);

impl<T> Receiver<T> {
    /// Iterate over received values, blocking until all senders disconnect.
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        std::iter::from_fn(move || self.recv().ok())
    }

    /// Block until a value arrives.
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    pub fn recv(&self) -> Result<T, RecvError> {
        self.0.recv_sync()
    }

    /// Await a value asynchronously (WASM only).
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    pub async fn recv_async(&self) -> Result<T, RecvError> {
        self.0.recv_async().await
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
    pub fn recv_timeout(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        self.0.recv_sync_timeout(deadline)
    }

    /// Iterate over currently-available values without blocking.
    pub fn try_iter(&self) -> impl Iterator<Item = T> + '_ {
        std::iter::from_fn(move || self.try_recv().ok())
    }

    /// Try to receive without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError`] if no value is available or senders are dropped.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.0.try_recv()
    }
}
