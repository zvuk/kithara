//! Shared audio worker — cooperative multi-track scheduler on a dedicated OS thread.

pub(crate) mod decoder_node;
pub(crate) mod handle;
pub(crate) mod hang_observer;
pub(crate) mod thread_wake;
mod traits;
pub(crate) mod types;

pub(crate) use traits::{AudioWorkerSource, apply_effects, flush_effects, reset_effects};
