#![forbid(unsafe_code)]

use kithara_platform::sync::Arc;

use super::FlushHub;

/// Wasm32 stub: no background flush worker exists in the browser (no
/// filesystem to flush). Index mutators always take the
/// `Flushable::flush` sync path because `is_started()` returns `false`.
#[derive(Default)]
pub(super) struct WorkerSlot;

impl WorkerSlot {
    pub(super) fn is_started(&self) -> bool {
        false
    }

    pub(super) fn shutdown_join(&mut self) {}

    pub(super) fn start_with(&self, _hub: &Arc<FlushHub>) {}
}
