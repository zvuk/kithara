mod api;
mod inference;
mod mel;
mod postprocess;
mod runtime;

pub use api::{BeatError, BeatThis, RawBeats};

pub const MEL_MODEL_BYTES: &[u8] = include_bytes!("../models/mel_spectrogram.onnx");
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!("../models/beat_this_small.onnx");
