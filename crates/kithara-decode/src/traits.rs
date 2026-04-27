//! Traits shared between decoder backends.

use std::{
    io::{ErrorKind, Read, Seek},
    num::NonZeroUsize,
    time::Duration,
};

use kithara_stream::{PendingReason, StreamReadError, VariantChangeError};
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{
    error::DecodeResult,
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Outcome of a [`DecoderInput::try_read`] call.
///
/// Mirrors the [`kithara_stream::ReadOutcome`] shape but operates on
/// the input-byte plane. `Bytes` carries a [`NonZeroUsize`] count;
/// `Pending` carries the typed [`PendingReason`] recovered from
/// `Stream`'s `impl Read` (or synthesised from `io::ErrorKind` for
/// non-stream inputs); `Eof` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputReadOutcome {
    Bytes(NonZeroUsize),
    Pending(PendingReason),
    Eof,
}

/// Outcome of a [`InnerDecoder::seek`] call.
///
/// `Landed.landed_at` is the position the decoder actually parked at
/// (often the granule boundary nearest the requested target — never
/// the requested target itself unless it coincides). `PastEof` carries
/// the decoder's known total duration so the caller can park at EOF
/// without rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderSeekOutcome {
    /// Decoder is now parked at `landed_at` / `landed_frame` /
    /// `landed_byte`. All three come from the decoder's own state
    /// and refer to the *same* point. `landed_byte`, when present,
    /// is the absolute byte offset in the underlying source where
    /// the next packet body begins — the pipeline plugs it into
    /// `Stream::seek` so we never recompute byte offsets from
    /// `frame × bytes_per_frame` heuristics on the consumer side.
    /// `None` is reserved for the rare case where the decoder
    /// successfully seeked but cannot expose a packet-aligned byte
    /// offset (e.g. `AudioFile` on a streaming MP3 whose seek-table
    /// is not yet built). For those, the pipeline relies on the
    /// producer-side `Stream::byte_position` updated by the
    /// decoder's own `Read::seek` calls — no extra arithmetic.
    Landed {
        landed_at: Duration,
        landed_frame: u64,
        landed_byte: Option<u64>,
    },
    /// Seek target was past the decoder's known duration. The decoder
    /// is parked at the end; the next `next_chunk` returns
    /// [`DecoderChunkOutcome::Eof`].
    PastEof { duration: Duration },
}

/// Outcome of an [`InnerDecoder::next_chunk`] call.
///
/// Mirrors [`kithara_stream::ReadOutcome`] / [`InputReadOutcome`] in
/// shape so every layer of the pipeline carries the same three-way
/// distinction (`progress | pending | terminal`). `Pending` carries
/// the typed [`PendingReason`] — typically
/// [`PendingReason::SeekPending`] when an in-flight seek aborted the
/// underlying read, or [`PendingReason::NotReady`] when the source
/// signalled transient backpressure.
#[derive(Debug)]
pub enum DecoderChunkOutcome {
    /// Decoded PCM chunk.
    Chunk(PcmChunk),
    /// Decoder is alive but produced no chunk this call. See
    /// [`PendingReason`] for the precise cause.
    Pending(PendingReason),
    /// Natural end of stream — no more chunks will be produced.
    Eof,
}

impl DecoderChunkOutcome {
    /// Borrow the inner [`PcmChunk`] when this outcome is `Chunk`.
    #[must_use]
    pub fn as_chunk(&self) -> Option<&PcmChunk> {
        match self {
            Self::Chunk(chunk) => Some(chunk),
            _ => None,
        }
    }

    /// Consume this outcome into the inner [`PcmChunk`] when it is
    /// `Chunk`. Returns `None` for `Pending` / `Eof`.
    #[must_use]
    pub fn into_chunk(self) -> Option<PcmChunk> {
        match self {
            Self::Chunk(chunk) => Some(chunk),
            _ => None,
        }
    }

    /// `true` when the outcome is [`Self::Chunk`].
    #[must_use]
    pub fn is_chunk(&self) -> bool {
        matches!(self, Self::Chunk(_))
    }

    /// `true` when the outcome is [`Self::Eof`].
    #[must_use]
    pub fn is_eof(&self) -> bool {
        matches!(self, Self::Eof)
    }

    /// `true` when the outcome is [`Self::Pending`].
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending(_))
    }
}

/// Combined trait for decoder input sources.
///
/// Supertrait combining `Read + Seek + Send + Sync`. Adds typed
/// [`try_read`] returning [`InputReadOutcome`] so decoders never
/// confuse "0 bytes" between EOF and `Pending(...)`.
/// `kithara_stream::Stream` packages its typed status (`SeekPending`,
/// `VariantChange`, `NotReady`/`Retry`) into `io::Error` payloads via
/// `impl Read for Stream`; the default `try_read` here downcasts those
/// payloads back into [`PendingReason`]. Arbitrary `Read + Seek`
/// sources (test cursors, fixtures) take the same default impl —
/// raw `io::Error` becomes [`StreamReadError::Source`], `Ok(0)` →
/// [`InputReadOutcome::Eof`].
pub trait DecoderInput: Read + Seek + Send + Sync {
    /// Typed read.
    ///
    /// # Errors
    ///
    /// Returns [`StreamReadError::Source`] for genuine source I/O
    /// failures. Status conditions (seek pending, variant change,
    /// data not ready) come back as `Ok(InputReadOutcome::Pending(...))`.
    fn try_read(&mut self, buf: &mut [u8]) -> Result<InputReadOutcome, StreamReadError> {
        match Read::read(self, buf) {
            Ok(0) => Ok(InputReadOutcome::Eof),
            Ok(n) => {
                Ok(NonZeroUsize::new(n).map_or(InputReadOutcome::Eof, InputReadOutcome::Bytes))
            }
            Err(e) => {
                if let Some(reason) = e
                    .get_ref()
                    .and_then(|src| src.downcast_ref::<PendingReason>())
                {
                    return Ok(InputReadOutcome::Pending(*reason));
                }
                if e.get_ref()
                    .and_then(|src| src.downcast_ref::<VariantChangeError>())
                    .is_some()
                {
                    return Ok(InputReadOutcome::Pending(PendingReason::VariantChange));
                }
                if e.kind() == ErrorKind::Interrupted {
                    return Ok(InputReadOutcome::Pending(PendingReason::NotReady));
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
    /// Returns [`DecoderChunkOutcome::Chunk`] with PCM data,
    /// [`DecoderChunkOutcome::Pending`] with a typed [`PendingReason`]
    /// when the underlying source aborted (seek pending, transient
    /// backpressure), or [`DecoderChunkOutcome::Eof`] at natural end
    /// of stream. Real decoder/codec failures surface as
    /// [`crate::error::DecodeError`] via the `Err` arm.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError`] if decoding fails.
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome>;

    /// Get the PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Seek to a time position.
    ///
    /// On success returns [`DecoderSeekOutcome::Landed`] with the
    /// authoritative landed position (often a granule boundary near
    /// the requested target — never assume `landed_at == pos`), or
    /// [`DecoderSeekOutcome::PastEof`] when the target is beyond the
    /// decoder's known duration.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError::SeekFailed`] if seeking is not supported
    /// or the position is invalid for reasons other than past-EOF.
    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome>;

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
