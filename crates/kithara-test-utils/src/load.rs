//! CPU contention started next to a test, so repeating one test reproduces the
//! scheduling pressure a full parallel run puts on it.

#[cfg(feature = "load")]
use std::{
    hint,
    sync::mpsc::{self, Sender, TryRecvError},
    thread::{self, JoinHandle},
};

/// Keeps every CPU busy while the test runs; dropping it stops the spinners.
/// Without the `load` feature it starts nothing.
pub struct LoadGuard {
    #[cfg(feature = "load")]
    stops: Vec<Sender<()>>,
    #[cfg(feature = "load")]
    spinners: Vec<JoinHandle<()>>,
}

impl LoadGuard {
    /// Start one spinning thread per available CPU.
    #[cfg(feature = "load")]
    #[must_use]
    pub fn start() -> Self {
        let count = thread::available_parallelism().map_or(1, usize::from);
        let (stops, spinners) = (0..count)
            .map(|_| {
                let (stop, stopped) = mpsc::channel::<()>();
                let spinner = thread::Builder::new()
                    .name("kithara-test-load".into())
                    .spawn(move || {
                        let _exclusion = crate::memory::exclude_current_thread();
                        while matches!(stopped.try_recv(), Err(TryRecvError::Empty)) {
                            hint::spin_loop();
                        }
                    })
                    .unwrap_or_else(|error| panic!("spawn a load spinner: {error}"));
                (stop, spinner)
            })
            .unzip();
        tracing::info!(spinners = count, "test CPU load started");
        Self { stops, spinners }
    }

    /// Start nothing: the `load` feature is off.
    #[cfg(not(feature = "load"))]
    #[must_use]
    pub const fn start() -> Self {
        Self {}
    }
}

#[cfg(feature = "load")]
impl Drop for LoadGuard {
    fn drop(&mut self) {
        self.stops.clear();
        for spinner in self.spinners.drain(..) {
            let _ = spinner.join();
        }
    }
}
