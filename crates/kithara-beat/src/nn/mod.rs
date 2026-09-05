mod api;
mod config;
mod consts;
mod inference;
mod mel;
#[cfg(any(
    feature = "embed-small-model",
    feature = "embed-full-model",
    feature = "embed-full-int8-model"
))]
mod models;
mod postprocess;
mod runtime;

pub use api::{BeatError, BeatThis};
pub use config::BeatConfig;
#[cfg(any(
    feature = "embed-small-model",
    feature = "embed-full-model",
    feature = "embed-full-int8-model"
))]
pub use models::{BEAT_MODEL_BYTES, BEAT_MODEL_TAG, MEL_MODEL_BYTES};
