use std::sync::atomic::{AtomicUsize, Ordering};

use kithara_platform::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct ConnectionMetrics {
    opened: Arc<AtomicUsize>,
}

impl ConnectionMetrics {
    pub(crate) fn connection_count(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn record_opened_connection(&self) {
        self.opened.fetch_add(1, Ordering::SeqCst);
    }
}
