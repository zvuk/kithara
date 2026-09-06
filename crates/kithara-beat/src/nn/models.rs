#[cfg(any(
    all(feature = "embed-small-model", feature = "embed-full-model"),
    all(feature = "embed-small-model", feature = "embed-full-int8-model"),
    all(feature = "embed-full-model", feature = "embed-full-int8-model"),
))]
compile_error!(
    "select one beat model: embed-small-model, embed-full-model or embed-full-int8-model"
);

/// ONNX mel model bytes for `BeatThis::builder().mel_model(..)`.
pub const MEL_MODEL_BYTES: &[u8] = include_bytes!("../../models/mel_spectrogram.onnx");

/// ONNX beat model bytes for `BeatThis::builder().beat_model(..)`.
#[cfg(feature = "embed-small-model")]
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!("../../models/beat_this_small.onnx");
/// Names the embedded model in the consumer's analysis fingerprint.
#[cfg(feature = "embed-small-model")]
pub const BEAT_MODEL_TAG: &str = "beat_this_small_v1";

/// ONNX beat model bytes for `BeatThis::builder().beat_model(..)`.
#[cfg(feature = "embed-full-model")]
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!(env!("KITHARA_BEAT_MODEL"));
/// Names the embedded model in the consumer's analysis fingerprint.
#[cfg(feature = "embed-full-model")]
pub const BEAT_MODEL_TAG: &str = "beat_this_full_v1";

/// ONNX beat model bytes for `BeatThis::builder().beat_model(..)`.
#[cfg(feature = "embed-full-int8-model")]
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!(env!("KITHARA_BEAT_MODEL"));
/// Names the embedded model in the consumer's analysis fingerprint.
#[cfg(feature = "embed-full-int8-model")]
pub const BEAT_MODEL_TAG: &str = "beat_this_full_int8_v1";
