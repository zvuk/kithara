#![deny(unsafe_code)]

//! Decoded-audio signal values and pure sample/time math.

mod buffer;
mod chunk;
mod coverage;
mod error;
mod fader;
mod sample;
mod session;
mod spec;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod time;
mod units;

pub use buffer::{InterleavedView, PlanarBuffer, PlanarView};
pub use chunk::{AudioChunk, AudioChunkInfo, SourceSpan};
pub use coverage::{CoverageRead, CoverageWrite, FrameCoverage, FrameSpan};
pub use error::SignalError;
pub use fader::FaderValue;
pub use sample::sanitize_sample;
pub use session::{OutputContext, SessionEpoch, SessionFrame, TransportRevision};
pub use spec::AudioSpec;
pub use units::{FrameCount, SampleCount};
mod consts;
