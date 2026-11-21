mod envelope;
mod ops;
mod waveform;

pub use waveform::WaveformAnalyzer;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod fold_tests;
