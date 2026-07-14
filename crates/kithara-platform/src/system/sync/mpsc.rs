pub use crate::system::errors::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};
use crate::time::Instant;

/// Create a new unbounded channel.
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (Sender(tx), Receiver(rx))
}

pub struct Sender<T>(std::sync::mpsc::Sender<T>);

impl<T> Sender<T> {
    /// Send a value synchronously.
    ///
    /// # Errors
    ///
    /// Returns [`SendError`] if the receiver has been dropped.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.0.send(value)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct Receiver<T>(std::sync::mpsc::Receiver<T>);

impl<T> Receiver<T> {
    delegate::delegate! {
        to self.0 {
            /// Iterate over received values, blocking until all senders disconnect.
            pub fn iter (& self) -> impl Iterator < Item = T > + '_;
            /// Iterate over currently-available values without blocking.
            pub fn try_iter (& self) -> impl Iterator < Item = T > + '_;
            /// Try to receive without blocking.
            ///
            /// # Errors
            ///
            /// Returns [`TryRecvError`] if no value is available or senders are dropped.
            pub fn try_recv (& self) -> Result < T , TryRecvError >;
        }
    }
    /// Block until a value arrives.
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all senders have been dropped.
    #[track_caller]
    pub fn recv(&self) -> Result<T, RecvError> {
        crate::no_block::forbid("mpsc::recv");
        self.0.recv()
    }

    /// Block until a value arrives or `deadline` elapses.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] when no value arrives before
    /// `deadline`, or [`RecvTimeoutError::Disconnected`] if all senders are
    /// dropped.
    #[track_caller]
    pub fn recv_timeout(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        crate::no_block::forbid("mpsc::recv_timeout");
        let now = Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        self.0.recv_timeout(remaining)
    }
}
