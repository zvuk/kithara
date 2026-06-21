/// Embedded ONNX model bytes for the `beat_this` small detector.
pub const MEL_MODEL_BYTES: &[u8] = include_bytes!("../models/mel_spectrogram.onnx");
pub const BEAT_MODEL_BYTES: &[u8] = include_bytes!("../models/beat_this_small.onnx");
