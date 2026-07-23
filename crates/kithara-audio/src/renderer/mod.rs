//! Shared audio renderer — cooperative multi-track scheduler on a dedicated OS thread.

mod gate;
pub(crate) mod handle;
mod load;
mod node;
mod source;
#[cfg(test)]
mod tests;

pub use gate::PreloadGate;
pub use handle::AudioWorkerHandle;
pub(crate) use handle::{TrackId, WorkerWakeBridge};
pub use load::{EngineLoad, EngineLoadSnapshot};
pub(crate) use node::{DecoderNode, TrackRegistration};
pub use source::AudioWorkerSource;
#[cfg(test)]
pub(crate) use source::MockAudioWorkerSource;
pub(crate) use source::{apply_effects, reset_effects};
#[cfg(test)]
pub(crate) use tests::MockSource;

pub use crate::runtime::ServiceClass;
pub(crate) use crate::runtime::{observer::HangWatchdogObserver, wake::ThreadWake};
