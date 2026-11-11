use std::mem;

use kithara_platform::{
    sync::Mutex,
    thread::{self, JoinHandle, ThreadId},
};

/// Failure to finish a native dispatcher thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ShutdownError {
    /// A dispatcher cannot wait for its own thread to exit.
    #[error("cannot join the current dispatcher thread")]
    CurrentThread,
    /// The scheduler panicked or an earlier join unwound.
    #[error("dispatcher thread panicked")]
    Panicked,
}

pub(super) struct Completion {
    id: ThreadId,
    state: Mutex<State>,
}

enum State {
    Running(JoinHandle<()>),
    Joined(Result<(), ShutdownError>),
}

impl Completion {
    pub(super) fn new(handle: JoinHandle<()>) -> Self {
        Self {
            id: handle.thread().id(),
            state: Mutex::new(State::Running(handle)),
        }
    }

    /// Refuse self-join before another caller's lock can block this thread.
    /// An unwinding join leaves a failed cached completion.
    pub(super) fn join(&self) -> Result<(), ShutdownError> {
        if thread::current().id() == self.id {
            return Err(ShutdownError::CurrentThread);
        }
        let mut state = self.state.lock();
        let result = match mem::replace(&mut *state, State::Joined(Err(ShutdownError::Panicked))) {
            State::Running(handle) => handle.join().map_err(|_| ShutdownError::Panicked),
            State::Joined(result) => result,
        };
        *state = State::Joined(result);
        result
    }

    /// Transfers an unjoined thread to the accepted runtime backstop on final Drop.
    pub(super) fn take_unjoined(&mut self) -> Option<JoinHandle<()>> {
        let mut state = self.state.lock();
        match mem::replace(&mut *state, State::Joined(Err(ShutdownError::Panicked))) {
            State::Running(thread) => Some(thread),
            State::Joined(result) => {
                *state = State::Joined(result);
                None
            }
        }
    }
}
