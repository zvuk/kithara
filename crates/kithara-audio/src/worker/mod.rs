//! Shared audio worker — cooperative multi-track scheduler on a dedicated OS thread.

pub(crate) mod decoder_node;
pub(crate) mod handle;
pub(crate) mod hang_observer;
pub(crate) mod thread_wake;
mod traits;
pub(crate) mod types;

pub use traits::AudioWorkerSource;
#[cfg(test)]
pub(crate) use traits::MockAudioWorkerSource;
pub(crate) use traits::{apply_effects, flush_effects, reset_effects};
