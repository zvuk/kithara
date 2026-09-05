#[cfg(feature = "dsp")]
mod dsp;
mod mark;
#[cfg(feature = "nn")]
mod nn;

#[cfg(feature = "dsp")]
pub use dsp::{SpectralBeats, Tempo, TempoError};
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;
pub use mark::{BeatMark, RawBeats};
#[cfg(any(
    feature = "embed-small-model",
    feature = "embed-full-model",
    feature = "embed-full-int8-model"
))]
pub use nn::{BEAT_MODEL_BYTES, BEAT_MODEL_TAG, MEL_MODEL_BYTES};
#[cfg(feature = "nn")]
pub use nn::{BeatConfig, BeatError, BeatThis};
