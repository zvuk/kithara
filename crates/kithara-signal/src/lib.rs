#![deny(unsafe_code)]

//! Decoded-audio signal values and pure sample/time math.

mod approach;
mod buffer;
mod chunk;
mod coverage;
mod error;
mod fader;
mod frame_pair;
mod revision;
mod sample;
mod session;
mod spec;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod time;
mod units;

pub use approach::ExponentialApproach;
pub use buffer::{InterleavedView, PlanarBuffer, PlanarView};
pub use chunk::{AudioChunk, AudioChunkInfo, SegmentId, SourceInverse, SourceSpan};
pub use coverage::{CoverageRead, CoverageWrite, FrameCoverage, FrameSpan};
pub use error::SignalError;
pub use fader::FaderValue;
pub use frame_pair::FramePair;
pub use revision::Revision;
pub use sample::sanitize_sample;
pub use session::{Bound, OutputContext, SessionEpoch, SessionFrame, TransportRevision};
pub use spec::AudioSpec;
pub use time::rescale_frame_ceil;
pub use units::{Count, FrameCount, Frames, SampleCount, Samples};
mod consts;
