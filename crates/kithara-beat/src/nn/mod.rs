mod api;
mod config;
mod inference;
mod mel;
#[cfg(feature = "embed-small-model")]
mod models;
mod postprocess;
mod runtime;

pub use api::{BeatError, BeatThis};
pub use config::BeatConfig;
#[cfg(feature = "embed-small-model")]
pub use models::{BEAT_MODEL_BYTES, MEL_MODEL_BYTES};
