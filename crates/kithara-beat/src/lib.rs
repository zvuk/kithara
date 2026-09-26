mod detector;
#[cfg(feature = "dsp")]
mod dsp;
mod grid;
mod mark;
#[cfg(feature = "nn")]
mod nn;

#[cfg(feature = "mock")]
pub use detector::BeatDetectorMock;
pub use detector::{BeatDetectError, BeatDetector};
#[cfg(feature = "dsp")]
pub use dsp::{SpectralBeats, Tempo, TempoError, TempoPatch, TempoPatchError};
pub use grid::{
    BeatGridError, BeatGridModel, BeatGridState, GridBeat, GridDownbeat, Meter, RawBeatGrid,
    SCHEMA_VERSION,
};
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
pub use mark::{BeatMark, RawBeats};
#[cfg(feature = "embed-model")]
pub use nn::{BEAT_MODEL_BYTES, BEAT_MODEL_TAG, MEL_MODEL_BYTES};
#[cfg(feature = "nn")]
pub use nn::{BeatConfig, BeatConfigPatch, BeatError, BeatThis};
mod consts;
