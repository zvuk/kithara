//! Traits shared between decoder backends.

use std::{
    io::{Read, Seek},
    time::Duration,
};

use kithara_stream::StreamReadError;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{
    error::DecodeResult,
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Combined trait for decoder input sources.
///
/// Supertrait combining `Read + Seek + Send + Sync`. Adds typed
/// [`try_read`] which lets decoders classify the read outcome
/// (`SeekPending`, `NotReady`, `VariantChange`, `Source`) without
/// matching `io::Error` strings. `kithara_stream::Stream` overrides
/// `try_read` to forward to its own typed `try_read`; arbitrary
/// `Read + Seek` sources (test cursors, fixtures) get the default
/// impl which wraps every `io::Error` as [`StreamReadError::Source`].
pub trait DecoderInput: Read + Seek + Send + Sync {
    /// Typed read. Forwards to [`Read::read`]; if the resulting
    /// [`io::Error`] carries a typed [`StreamReadError`] payload (set
    /// by `Stream`'s `impl Read`), we recover the precise variant via
    /// `downcast` instead of collapsing every error into
    /// [`StreamReadError::Source`].
    ///
    /// # Errors
    ///
    /// Returns the typed [`StreamReadError`] variant — `SeekPending` /
    /// `VariantChange` recovered from the wrapped [`io::Error`], or
    /// [`StreamReadError::Source`] for unrelated I/O failures.
    fn try_read(&mut self, buf: &mut [u8]) -> Result<usize, StreamReadError> {
        match Read::read(self, buf) {
            Ok(n) => Ok(n),
            Err(e) => {
                if let Some(inner) = e
                    .get_ref()
                    .and_then(|src| src.downcast_ref::<StreamReadError>())
                {
                    match inner {
                        StreamReadError::SeekPending => return Err(StreamReadError::SeekPending),
                        StreamReadError::VariantChange => {
                            return Err(StreamReadError::VariantChange);
                        }
                        StreamReadError::NotReady | StreamReadError::Source(_) => {}
                    }
                }
                Err(StreamReadError::Source(e))
            }
        }
    }
}

impl<T: Read + Seek + Send + Sync + ?Sized> DecoderInput for T {}

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
