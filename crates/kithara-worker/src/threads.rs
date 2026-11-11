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
                finished: false,
            }),
            finished: Condvar::default(),
        });
        state.handles.retain(|thread| !thread.state.lock().finished);
        state.handles.push(Arc::clone(&thread));
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

    pub(crate) fn is_current(&self) -> bool {
        let current = thread::current().id();
        self.state
            .lock()
            .handles
            .iter()
            .any(|thread| thread.id == current)
    }

    #[kithara::hang_watchdog]
    pub(crate) fn join(&self) {
        let handles = self.state.lock().handles.clone();
        for thread in handles {
            hang_tick!();
            thread.join();
            hang_reset!();
        }
    }
}

pub(crate) struct ThreadJoin {
    id: ThreadId,
    state: Mutex<JoinState>,
    finished: Condvar,
}

struct JoinState {
    handle: Option<JoinHandle<()>>,
    finished: bool,
}

impl ThreadJoin {
    #[kithara::hang_watchdog]
    pub(crate) fn join(&self) {
        if self.id == thread::current().id() {
            return;
        }
        let mut state = self.state.lock();
        if state.finished {
            return;
        }
        kithara_platform::no_block::forbid_bridged(None);
        if let Some(handle) = state.handle.take() {
            drop(state);
            let result = handle.join();
            self.state.lock().finished = true;
            self.finished.notify_all();
            drop(result);
        } else {
            while !state.finished {
                hang_tick!();
                state = self.finished.wait(state);
                hang_reset!();
            }
        }
    }
}
