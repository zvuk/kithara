#[cfg(not(target_arch = "wasm32"))]
use kithara::platform::sync::Condvar;
use kithara::platform::sync::Mutex;

use crate::types::FfiError;

enum State<T> {
    Uninitialized,
    Initializing,
    Ready(T),
}

pub(crate) struct Lifecycle<T> {
    state: Mutex<State<T>>,
    #[cfg(not(target_arch = "wasm32"))]
    changed: Condvar,
}

impl<T> Default for Lifecycle<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::Uninitialized),
            #[cfg(not(target_arch = "wasm32"))]
            changed: Condvar::default(),
        }
    }
}

impl<T> Lifecycle<T> {
    pub(crate) fn initialize(
        &self,
        build: impl FnOnce() -> Result<T, FfiError>,
    ) -> Result<(), FfiError> {
        {
            let mut state = self.state.lock();
            match &*state {
                State::Uninitialized => *state = State::Initializing,
                State::Initializing => return Err(FfiError::InitializationInProgress),
                State::Ready(_) => return Err(FfiError::AlreadyInitialized),
            }
        }

        self.finish_initialization(build)
    }

    /// Join a native first-use initialization or build the default host once.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn ensure_initialized(
        &self,
        build: impl FnOnce() -> Result<T, FfiError>,
    ) -> Result<(), FfiError> {
        let mut state = self.state.lock();
        loop {
            match &*state {
                State::Ready(_) => return Ok(()),
                State::Initializing => state = self.changed.wait(state),
                State::Uninitialized => {
                    *state = State::Initializing;
                    break;
                }
            }
        }
        drop(state);
        self.finish_initialization(build)
    }

    fn finish_initialization(
        &self,
        build: impl FnOnce() -> Result<T, FfiError>,
    ) -> Result<(), FfiError> {
        let result = match build() {
            Ok(value) => {
                *self.state.lock() = State::Ready(value);
                Ok(())
            }
            Err(error) => {
                *self.state.lock() = State::Uninitialized;
                Err(error)
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        self.changed.notify_all();
        result
    }

    pub(crate) fn with_ready<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, FfiError> {
        let state = self.state.lock();
        match &*state {
            State::Ready(value) => Ok(f(value)),
            State::Uninitialized => Err(FfiError::NotInitialized),
            State::Initializing => Err(FfiError::InitializationInProgress),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn with_ready_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, FfiError> {
        let mut state = self.state.lock();
        match &mut *state {
            State::Ready(value) => Ok(f(value)),
            State::Uninitialized => Err(FfiError::NotInitialized),
            State::Initializing => Err(FfiError::InitializationInProgress),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        thread,
    };

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn failed_initialization_can_retry_but_ready_cannot_reinitialize() {
        let lifecycle = Lifecycle::default();
        assert!(matches!(
            lifecycle.with_ready(|value: &u32| *value),
            Err(FfiError::NotInitialized)
        ));
        assert!(matches!(
            lifecycle.initialize(|| Err(FfiError::InvalidArgument {
                reason: "rejected".to_owned(),
            })),
            Err(FfiError::InvalidArgument { .. })
        ));
        lifecycle.initialize(|| Ok(7)).expect("retry succeeds");
        assert_eq!(lifecycle.with_ready(|value| *value).unwrap(), 7);
        assert!(matches!(
            lifecycle.initialize(|| Ok(9)),
            Err(FfiError::AlreadyInitialized)
        ));
    }

    #[kithara::test]
    fn concurrent_initialization_is_rejected_while_builder_owns_no_lock() {
        let lifecycle = Arc::new(Lifecycle::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_lifecycle = Arc::clone(&lifecycle);
        let worker = thread::spawn(move || {
            worker_lifecycle.initialize(|| {
                entered_tx.send(()).expect("signal builder entry");
                release_rx.recv().expect("release builder");
                Ok(7)
            })
        });
        entered_rx.recv().expect("builder entered");
        assert!(matches!(
            lifecycle.initialize(|| Ok(9)),
            Err(FfiError::InitializationInProgress)
        ));
        release_tx.send(()).expect("release builder");
        worker.join().expect("initializer thread").unwrap();
        assert_eq!(lifecycle.with_ready(|value| *value).unwrap(), 7);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn concurrent_default_ensure_uses_the_ready_host() {
        let lifecycle = Arc::new(Lifecycle::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_lifecycle = Arc::clone(&lifecycle);
        let first = thread::spawn(move || {
            first_lifecycle.initialize(|| {
                entered_tx.send(()).expect("signal builder entry");
                release_rx.recv().expect("release builder");
                Ok(7)
            })
        });
        entered_rx.recv().expect("builder entered");

        let second_lifecycle = Arc::clone(&lifecycle);
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let second = thread::spawn(move || {
            attempt_tx.send(()).expect("signal ensure attempt");
            second_lifecycle.ensure_initialized(|| Ok(9))
        });
        attempt_rx.recv().expect("ensure attempted");
        release_tx.send(()).expect("release builder");

        first.join().expect("initializer thread").unwrap();
        second.join().expect("ensure thread").unwrap();
        assert_eq!(lifecycle.with_ready(|value| *value).unwrap(), 7);
        lifecycle
            .ensure_initialized(|| panic!("ready host must not rebuild"))
            .unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn failed_default_ensure_allows_a_later_attempt() {
        let lifecycle = Lifecycle::default();
        assert!(matches!(
            lifecycle.ensure_initialized(|| Err(FfiError::InvalidArgument {
                reason: "rejected".to_owned(),
            })),
            Err(FfiError::InvalidArgument { .. })
        ));
        lifecycle.ensure_initialized(|| Ok(7)).unwrap();
        assert_eq!(lifecycle.with_ready(|value| *value).unwrap(), 7);
    }
}
