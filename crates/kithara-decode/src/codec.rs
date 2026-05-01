//! [`FrameCodec`] trait — codec-side half of the unified architecture.

use std::time::Duration;

use kithara_bufpool::PcmBuf;

use crate::{demuxer::TrackInfo, error::DecodeResult, types::PcmSpec};

/// Frame-level codec contract paired with a [`crate::demuxer::Demuxer`] in
/// `UniversalDecoder<D, C>`.
///
/// Implementations consume one demuxed frame at a time and write
/// interleaved f32 PCM into the caller-provided pool buffer (`out`).
/// They never see container bytes — container parsing is the demuxer's
/// job. They never allocate their own output `Vec<f32>` — output flows
/// through the injected `PcmPool`, which keeps the hot path zero-alloc
/// once the pool is warm.
pub(crate) trait FrameCodec: Send + 'static {
    /// Decode one demuxed frame, writing interleaved f32 samples into
    /// `out` (which the caller acquired from the shared `PcmPool`).
    /// Returns the frame count actually written (each frame is
    /// `spec.channels` samples). `0` means the codec consumed the input
    /// but produced no PCM (warm-up packet, codec backpressure, etc.) —
    /// caller should pull the next frame.
    ///
    /// `frame_data` is the raw bytes for this frame. `pts` is the
    /// presentation time supplied by the demuxer; codecs may use it
    /// for diagnostics — decoded sample count + sample rate are the
    /// authoritative duration source.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::DecodeError`] when the underlying decoder
    /// produces an unrecoverable error or when growing `out` would
    /// exceed the pool's byte budget.
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        out: &mut PcmBuf,
    ) -> DecodeResult<u32>;

    /// Reset internal codec state — called after seek.
    fn flush(&mut self);

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

    /// PCM output specification.
    fn spec(&self) -> PcmSpec;
}
