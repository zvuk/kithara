#![forbid(unsafe_code)]

/// Wasm32 stub: no background flush worker exists in the browser (no
/// filesystem to flush). `signal_or_flush_sync` always takes the
/// `Flushable::flush` sync path because `is_started()` returns `false`.
pub(super) struct WorkerSlot;

impl WorkerSlot {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn is_started(&self) -> bool {
        false
    }
}
