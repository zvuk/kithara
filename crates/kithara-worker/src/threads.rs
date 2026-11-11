use std::io;

use kithara_platform::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle, ThreadId},
};
use kithara_test_macros as kithara;

#[derive(Default)]
pub(crate) struct Threads {
    state: Mutex<ThreadState>,
}

#[derive(Default)]
struct ThreadState {
    handles: Vec<Arc<ThreadJoin>>,
    closed: bool,
}

impl Threads {
    fn spawn_with(&self, spawn: impl FnOnce() -> JoinHandle<()>) -> Option<Arc<ThreadJoin>> {
        let mut state = self.state.lock();
        if state.closed {
            return None;
        }
        let handle = spawn();
        let thread = Arc::new(ThreadJoin {
            id: handle.thread().id(),
            state: Mutex::new(JoinState {
                handle: Some(handle),
                result: None,
            }),
            finished: Condvar::default(),
        });
        state
            .handles
            .retain(|thread| thread.state.lock().result.is_none());
        state.handles.push(Arc::clone(&thread));
        drop(state);
        Some(thread)
    }

    pub(crate) fn spawn(
        &self,
        name: String,
        job: impl FnOnce() + Send + 'static,
    ) -> Option<Arc<ThreadJoin>> {
        self.spawn_with(|| thread::spawn_named(name, job))
    }

    pub(crate) fn spawn_pool(
        &self,
        name: String,
        job: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        self.spawn_with(|| thread::spawn_pool_thread(name, job))
            .map(|_| ())
            .ok_or_else(|| io::Error::from(io::ErrorKind::Interrupted))
    }

    pub(crate) fn close(&self) {
        self.state.lock().closed = true;
    }

    #[kithara::hang_watchdog]
    pub(crate) fn join(&self) {
        let handles = self.state.lock().handles.clone();
        for thread in handles {
            hang_tick!();
            // A panicked or current thread has nothing left to wait for.
            thread.join().ok();
            hang_reset!();
        }
    }
}

/// Failure to finish a native worker thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ShutdownError {
    /// A thread cannot wait for itself to exit.
    #[error("cannot join the current thread")]
    CurrentThread,
    /// The thread panicked or an earlier join unwound.
    #[error("worker thread panicked")]
    Panicked,
}

pub(crate) struct ThreadJoin {
    id: ThreadId,
    state: Mutex<JoinState>,
    finished: Condvar,
}

struct JoinState {
    handle: Option<JoinHandle<()>>,
    result: Option<Result<(), ShutdownError>>,
}

/// Publishes a failed completion if the join unwinds before it records one.
struct Publish<'a> {
    thread: &'a ThreadJoin,
    result: Result<(), ShutdownError>,
}

impl Drop for Publish<'_> {
    fn drop(&mut self) {
        self.thread.state.lock().result = Some(self.result);
        self.thread.finished.notify_all();
    }
}

impl ThreadJoin {
    /// Wait for the thread and its TLS teardown. Concurrent callers wait for
    /// the same exit, and every later call returns the recorded result.
    #[kithara::hang_watchdog]
    pub(crate) fn join(&self) -> Result<(), ShutdownError> {
        if self.id == thread::current().id() {
            return Err(ShutdownError::CurrentThread);
        }
        let mut state = self.state.lock();
        if let Some(handle) = state.handle.take() {
            drop(state);
            let mut publish = Publish {
                thread: self,
                result: Err(ShutdownError::Panicked),
            };
            publish.result = handle.join().map_err(|_| ShutdownError::Panicked);
            return publish.result;
        }
        while state.result.is_none() {
            hang_tick!();
            state = self.finished.wait(state);
            hang_reset!();
        }
        let result = state.result;
        drop(state);
        result.unwrap_or(Err(ShutdownError::Panicked))
    }
}
