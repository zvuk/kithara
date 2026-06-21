mod api;
mod inference;
mod mel;
#[cfg(feature = "embed-small-model")]
mod models;
mod postprocess;
mod runtime;

pub use api::{BeatError, BeatThis, RawBeats};
#[cfg(feature = "embed-small-model")]
pub use models::{BEAT_MODEL_BYTES, MEL_MODEL_BYTES};
