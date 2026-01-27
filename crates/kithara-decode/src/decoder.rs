//! Generic decoder trait for audio decoding.
//!
//! Allows using different decoders (Symphonia for production, mock for tests).

use crate::{DecodeResult, PcmChunk, PcmSpec};

/// Generic audio decoder trait (internal).
///
/// Implementations:
/// - `SymphoniaDecoder` - production decoder using Symphonia (AAC/MP3/FLAC/etc)
/// - `MockDecoder` - test decoder for unit tests
pub trait InnerDecoder: Send + 'static {
    /// Decode next chunk of audio.
    ///
    /// Returns:
    /// - `Ok(Some(chunk))` - successfully decoded chunk
    /// - `Ok(None)` - end of stream
    /// - `Err(e)` - decode error
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>>;

    /// Get PCM specification (sample rate, channels).
    fn spec(&self) -> PcmSpec;
}
