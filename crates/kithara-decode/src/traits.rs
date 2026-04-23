//! Traits shared between decoder backends.

use std::{
    io::{Read, Seek},
    time::Duration,
};

#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{
    error::DecodeResult,
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Combined trait for decoder input sources.
///
/// This supertrait combines `Read + Seek + Send + Sync` into a single trait
/// that can be used as a trait object (`Box<dyn DecoderInput>`).
///
/// A blanket implementation is provided for all types satisfying the bounds.
pub(crate) trait DecoderInput: Read + Seek + Send + Sync {}

impl<T: Read + Seek + Send + Sync> DecoderInput for T {}

/// Trait for runtime-polymorphic audio decoders.
///
/// This trait is used by kithara-audio for dynamic dispatch when the
/// decoder type is determined at runtime (e.g., based on media info).
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = InnerDecoderMock))]
pub trait InnerDecoder: Send + 'static {
    /// Decode the next chunk of PCM data.
    ///
    /// Returns `Ok(Some(chunk))` with PCM data, or `Ok(None)` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError`] if decoding fails.
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>>;

    /// Get the PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Seek to a time position.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError::SeekFailed`] if seeking is not supported
    /// or the position is invalid.
    fn seek(&mut self, pos: Duration) -> DecodeResult<()>;

    /// Update the byte length reported to the underlying media source.
    ///
    /// For HLS streams, the total length becomes known after metadata
    /// calculation. Call this before seeking so the decoder can compute
    /// correct seek deltas.
    fn update_byte_len(&self, len: u64);

    /// Get total duration from track metadata.
    ///
    /// Returns `None` if duration cannot be determined.
    fn duration(&self) -> Option<Duration>;

    /// Get track metadata (title, artist, album, artwork).
    ///
    /// Returns default metadata if not available.
    fn metadata(&self) -> TrackMetadata {
        TrackMetadata::default()
    }
}
