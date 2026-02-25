//! Thread-like primitives for sync code.
//!
//! This module provides a small compatibility layer used across the workspace.
//! On native it reuses `std::thread`; on wasm it uses `rayon` work items and
//! a synchronous result channel so caller code can keep the same patterns.

#[cfg(not(target_arch = "wasm32"))]
pub use std::thread::{JoinHandle, sleep, spawn, yield_now};
#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Duration;
#[cfg(target_arch = "wasm32")]
use std::{
    any::Any,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

#[cfg(target_arch = "wasm32")]
use crate::time::Instant;

#[cfg(target_arch = "wasm32")]
pub type Duration = std::time::Duration;

#[cfg(target_arch = "wasm32")]
pub type Result<T> = std::result::Result<T, Box<dyn Any + Send + 'static>>;

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct JoinHandle<T> {
    receiver: mpsc::Receiver<Result<T>>,
    finished: Arc<AtomicBool>,
}

#[cfg(target_arch = "wasm32")]
impl<T> JoinHandle<T> {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn join(self) -> Result<T> {
        self.receiver
            .recv()
            .unwrap_or_else(|_| panic!("spawned task ended before sending completion result"))
    }
}

#[cfg(target_arch = "wasm32")]
pub fn yield_now() {
    core::hint::spin_loop();
}

#[cfg(target_arch = "wasm32")]
pub fn sleep(duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        yield_now();
    }
}

#[cfg(target_arch = "wasm32")]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Result<T>>();
    let finished = Arc::new(AtomicBool::new(false));
    let finished2 = Arc::clone(&finished);

    rayon::spawn(move || {
        let result = catch_unwind(AssertUnwindSafe(f));
        let _ = tx.send(result);
        finished2.store(true, Ordering::Release);
    });

    JoinHandle {
        receiver: rx,
        finished,
    }
}
