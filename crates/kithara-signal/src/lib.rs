#![deny(unsafe_code)]

//! Decoded-audio signal values and pure sample/time math.

mod chunk;
mod error;
mod fader;
mod interleaved;
mod planar;
mod sample;
mod spec;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod time;
mod units;

pub use chunk::{
    AudioChunk, AudioChunkInfo, pack_render_revision, render_rate_revision,
    render_warp_map_revision,
};
pub use error::SignalError;
pub use fader::FaderValue;
pub use interleaved::InterleavedView;
pub use planar::{PlanarBuffer, PlanarView};
pub use sample::sanitize_sample;
pub use spec::AudioSpec;
pub use units::{FrameCount, SampleCount};
