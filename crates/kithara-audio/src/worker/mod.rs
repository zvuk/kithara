//! Shared audio worker — cooperative multi-track scheduler on a dedicated OS thread.

pub(crate) mod decoder;
pub(crate) mod handle;
pub(crate) mod hang_observer;
mod load;
pub(crate) mod preload_gate;
pub(crate) mod thread_wake;
mod traits;
pub(crate) mod types;

pub use load::{EngineLoad, EngineLoadSnapshot};
pub use preload_gate::PreloadGate;
pub use traits::AudioWorkerSource;
#[cfg(test)]
pub(crate) use traits::MockAudioWorkerSource;
pub(crate) use traits::{apply_effects, drain_effects, reset_effects};
