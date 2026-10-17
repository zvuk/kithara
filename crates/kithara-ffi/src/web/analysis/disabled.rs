use js_sys::Function;
use kithara::queue::TrackId;
use wasm_bindgen::JsValue;

use crate::pools::{FfiQueueControl, FfiResourceConfig, Pools};

#[derive(Clone, Default)]
pub(crate) struct AnalysisRoute;

impl AnalysisRoute {
    pub(crate) const fn dispatch(&self, _scope: Option<&str>, _data: &JsValue) -> bool {
        false
    }

    pub(crate) fn set(&self, _func: Function) {}
}

pub(crate) struct AnalysisRuns;

impl AnalysisRuns {
    pub(crate) fn new(_pools: Pools) -> Self {
        Self
    }

    pub(crate) const fn clear(&mut self) {}

    pub(crate) const fn reconcile(&mut self, _queue: &FfiQueueControl) {}

    pub(crate) fn start_queued<F>(
        &mut self,
        _queue: &FfiQueueControl,
        _id: TrackId,
        _request_id: u32,
        _config_for: F,
    ) -> Result<(), String>
    where
        F: FnOnce(&str) -> Option<FfiResourceConfig>,
    {
        Err("analysis is not enabled".to_owned())
    }
}
