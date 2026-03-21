//! Shared audio worker — cooperative multi-track scheduler on a dedicated OS thread.

pub(crate) mod handle;
pub(crate) mod thread_wake;
mod traits;
pub(crate) mod types;
pub(crate) mod wake;

pub(crate) use traits::{
    AudioCommand, AudioWorkerSource, apply_effects, flush_effects, reset_effects,
};
