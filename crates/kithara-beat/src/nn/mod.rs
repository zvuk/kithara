mod api;
mod config;
mod consts;
mod inference;
mod mel;
#[cfg(feature = "embed-model")]
mod models;
mod postprocess;
mod runtime;

pub use api::{BeatError, BeatThis};
pub use config::BeatConfig;
#[cfg(feature = "embed-model")]
pub use models::{BEAT_MODEL_BYTES, BEAT_MODEL_TAG, MEL_MODEL_BYTES};
