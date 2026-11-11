#![forbid(unsafe_code)]

use std::{convert::Infallible, marker::PhantomData};

use kithara_platform::sync::Arc;

use super::FlushHub;
use crate::error::AssetsResult;

/// Wasm32 stub: no background flush worker exists in the browser (no
/// filesystem to flush). Index mutators always take the
/// `Flushable::flush` sync path because `is_started()` returns `false`.
#[derive(Default)]
pub(super) struct WorkerSlot;

impl WorkerSlot {
    pub(super) fn is_started(&self) -> bool {
        false
    }

    pub(super) fn start_with(&self, _hub: &Arc<FlushHub>) {}
}

/// Wasm32 stub: the browser has no filesystem, so no index has a file to land
/// in and this target cannot be built.
pub(crate) struct IndexTarget<S>(Infallible, PhantomData<fn(&S)>);

impl<S> IndexTarget<S> {
    pub(crate) fn publish(&self, _state: &S, _durable: bool) -> AssetsResult<()> {
        match self.0 {}
    }
}
