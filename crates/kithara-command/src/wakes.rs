use std::{
    sync::atomic::{AtomicUsize, Ordering},
    task::{Wake, Waker},
};

use kithara_platform::sync::Arc;

/// An executor's waker that counts its wakes.
#[derive(Default)]
pub(crate) struct Wakes(AtomicUsize);

impl Wakes {
    /// Wakes so far.
    pub(crate) fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// A waker and the count of its wakes.
pub(crate) fn waker() -> (Waker, Arc<Wakes>) {
    let wakes = Arc::new(Wakes::default());
    (Waker::from(Arc::clone(&wakes)), wakes)
}
