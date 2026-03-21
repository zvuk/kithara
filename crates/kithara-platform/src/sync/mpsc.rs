//! Platform-optimal multi-producer single-consumer channel.
//!
//! * **Native** — [`std::sync::mpsc`].
//! * **WASM** — [`wasm_safe_thread::mpsc`] (async-capable on Web Workers).

#[cfg(not(target_arch = "wasm32"))]
pub use std::sync::mpsc::{RecvError, SendError, TryRecvError};

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
}

#[cfg(target_arch = "wasm32")]
use wasm_safe_thread::mpsc as wasm_mpsc;
#[cfg(target_arch = "wasm32")]
pub use wasm_safe_thread::mpsc::{RecvError, SendError, TryRecvError};

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

    /// Await a value asynchronously (WASM only).
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    pub async fn recv_async(&self) -> Result<T, RecvError> {
        self.0.recv_async().await
    }
}
