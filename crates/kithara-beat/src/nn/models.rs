/// Embedded ONNX model bytes and the tag naming what produced a grid.
///
/// Exactly one model feature may be on: the tag travels in the analysis
/// fingerprint, so a build cannot silently serve a grid another model made.
#[cfg(any(
    all(feature = "embed-small-model", feature = "embed-full-model"),
    all(feature = "embed-small-model", feature = "embed-full-int8-model"),
    all(feature = "embed-full-model", feature = "embed-full-int8-model"),
))]
compile_error!(
    "select one beat model: embed-small-model, embed-full-model or embed-full-int8-model"
);

pub const MEL_MODEL_BYTES: &[u8] = include_bytes!("../../models/mel_spectrogram.onnx");

#[cfg(feature = "embed-small-model")]
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!("../../models/beat_this_small.onnx");
#[cfg(feature = "embed-small-model")]
pub const BEAT_MODEL_TAG: &str = "beat_this_small_v1";

#[cfg(feature = "embed-full-model")]
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!(env!("KITHARA_BEAT_MODEL"));
#[cfg(feature = "embed-full-model")]
pub const BEAT_MODEL_TAG: &str = "beat_this_full_v1";

#[cfg(feature = "embed-full-int8-model")]
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!(env!("KITHARA_BEAT_MODEL"));
#[cfg(feature = "embed-full-int8-model")]
pub const BEAT_MODEL_TAG: &str = "beat_this_full_int8_v1";
