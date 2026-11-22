//! A track's waveform: the rendered model, its byte codec, and the progressive
//! analyzer that produces it.
//!
//! The model and its codec carry no DSP, so a consumer that receives a served
//! waveform needs no analyzer to read one.

#![forbid(unsafe_code)]

#[cfg(feature = "dsp")]
mod analyzer;
mod band;
mod bucket;
mod error;
mod params;
mod resume;

#[cfg(feature = "dsp")]
pub use analyzer::WaveformAnalyzer;
pub(crate) use band::Band;
pub use bucket::{Bucket, MAX_BUCKETS, WAVEFORM_BYTES_VERSION, Waveform, WaveformError};
pub use error::AnalyzerError;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
pub use params::AnalysisParams;
pub use resume::{WaveformPartialResume, WaveformResume};
mod consts;
