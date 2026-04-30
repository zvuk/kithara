//! [`FrameCodec`] trait — codec-side half of the unified architecture.

use std::time::Duration;

use crate::{demuxer::TrackInfo, error::DecodeResult, types::PcmSpec};

/// Output of one frame decode.
///
/// Interleaved PCM samples; frame count = `samples.len() / channels`.
#[derive(Debug)]
pub struct DecodedFrame {
    pub samples: Vec<f32>,
    pub frames: u32,
}

/// Frame-level codec contract paired with a [`super::super::Demuxer`] in
/// `UniversalDecoder<D, C>`.
///
/// Implementations consume one demuxed frame at a time and produce
/// interleaved f32 PCM. They never see container bytes — container
/// parsing is the demuxer's job.
pub trait FrameCodec: Send + 'static {
    /// Construct a codec from track-level metadata produced by the
    /// demuxer (`extra_data` carries codec-specific config such as AAC
    /// `AudioSpecificConfig` or FLAC `STREAMINFO`).
    ///
    /// # Errors
    ///
    /// Returns a [`crate::DecodeError`] when the codec rejects the
    /// track (unsupported codec id, malformed extra-data, etc.).
    fn open(track: &TrackInfo) -> DecodeResult<Self>
    where
        Self: Sized;

    /// Decode one demuxed frame.
    ///
    /// `frame_data` is the raw bytes for this frame. `pts` is the
    /// presentation time supplied by the demuxer; codecs may use it
    /// for diagnostics — decoded sample count + sample rate are the
    /// authoritative duration source.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::DecodeError`] when the underlying decoder
    /// produces an unrecoverable error.
    fn decode_frame(&mut self, frame_data: &[u8], pts: Duration) -> DecodeResult<DecodedFrame>;

    /// Reset internal codec state — called after seek.
    fn flush(&mut self);

    /// PCM output specification.
    fn spec(&self) -> PcmSpec;
}
