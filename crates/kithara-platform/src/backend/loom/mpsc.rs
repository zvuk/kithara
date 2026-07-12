pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};

use crate::common::time::Instant;

#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = ::loom::sync::mpsc::channel();
    (Sender(sender), Receiver(receiver))
}

pub struct Sender<T>(::loom::sync::mpsc::Sender<T>);

impl<T> Sender<T> {
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.0.send(value)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct Receiver<T>(::loom::sync::mpsc::Receiver<T>);

impl<T> Receiver<T> {
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        core::iter::from_fn(|| self.recv().ok())
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        crate::no_block::forbid("mpsc::recv");
        self.0.recv()
    }

    pub fn recv_timeout(&self, _deadline: Instant) -> Result<T, RecvTimeoutError> {
        panic!("timed channel receives require the flash backend when loom is enabled");
    }

    pub fn try_iter(&self) -> impl Iterator<Item = T> + '_ {
        core::iter::from_fn(|| self.try_recv().ok())
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.0.try_recv()
    }
}
