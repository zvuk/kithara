//! [`FrameCodec`] trait — codec-side half of the unified architecture.

use kithara_bufpool::PcmBuf;
use kithara_platform::time::Duration;

use crate::{error::DecodeResult, types::PcmSpec};

/// Frame-level codec contract paired with a [`crate::demuxer::Demuxer`] in
/// `ComposedDecoder<D, C>`.
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

    /// Reset internal codec state — called after seek. Backends that
    /// can fail to reset (Android `AMediaCodec_flush`) propagate the
    /// error through `DecodeResult` so the seek path can surface it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::DecodeError`] when the codec rejects the
    /// reset.
    fn flush(&mut self) -> DecodeResult<()>;

    /// PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Codec-owned playback contract — currently the captured
    /// [`crate::GaplessInfo`] (encoder priming + trailing padding in
    /// PCM frames). Default returns the empty contract; per-platform
    /// implementations override when their codec exposes
    /// priming numbers (Apple `kAudioConverterPrimeInfo`, Symphonia
    /// `AudioDecoderOptions::gapless`, Android via the demuxer's
    /// container probe).
    ///
    /// `ComposedDecoder<D, C>` forwards this through
    /// [`crate::Decoder::track_info`] so the audio pipeline can build
    /// a [`crate::GaplessTrimmer`] without knowing the concrete codec
    /// type.
    fn track_info(&self) -> crate::DecoderTrackInfo {
        crate::DecoderTrackInfo::default()
    }
}
