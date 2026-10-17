use std::{
    io::{ErrorKind, Read, Seek},
    num::NonZeroUsize,
};

use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioSpec};
use kithara_stream::{
    AudioCodec, NotReadyCause, PendingReason, PrerollHint, StreamPending, StreamReadError,
    VariantChangeError,
};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

use crate::{
    PcmOrigin,
    error::DecodeResult,
    types::{BlenderProfile, GaplessProfile, TrackMetadata},
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

/// Outcome of a [`Decoder::seek`] call.
///
/// `Landed.landed_at` is the position the decoder actually parked at
/// (often the granule boundary nearest the requested target — never
/// the requested target itself unless it coincides). `PastEof` carries
/// the decoder's known total duration so the caller can park at EOF
/// without rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderSeekOutcome {
    /// Accepted seek with no landing coordinate yet. Drive [`Decoder::prepare_seek`]
    /// before consuming prepared PCM.
    Pending(PendingReason),
    /// Successful seek coordinates from one decoder state, all naming the same landing point.
    /// `landed_byte` is the absolute next-packet body offset; the pipeline uses it without
    /// frame/byte heuristics. If unavailable (such as an unbuilt streaming MP3 seek table),
    /// the producer's decoder-driven `Read::seek` updates `Stream::byte_position` instead.
    Landed {
        landed_at: Duration,
        landed_frame: u64,
        landed_byte: Option<u64>,
        /// Hint from the demuxer: an earlier byte position the source must
        /// still hold so the decoder can warm MDCT overlap-add state before
        /// emitting user-visible samples. `NotNeeded` until a demuxer
        /// computes a meaningful pre-roll hint (file backend by byte offset
        /// back, HLS by prev segment start); consumed by the audio pipeline
        /// to warm MDCT overlap-add state before emitting user-visible
        /// chunks.
        preroll: PrerollHint,
    },
    /// Seek target was past the decoder's known duration. The decoder
    /// is parked at the end; the next `next_chunk` returns
    /// [`DecoderChunkOutcome::Eof`].
    PastEof { duration: Duration },
}

/// Outcome of an [`Decoder::next_chunk`] call.
///
/// Mirrors [`kithara_stream::ReadOutcome`] / [`InputReadOutcome`] in
/// shape so every layer of the pipeline carries the same three-way
/// distinction (`progress | pending | terminal`). `Pending` carries
/// the typed [`PendingReason`] for transient backpressure or a
/// cross-variant boundary.
#[derive(Debug)]
pub enum DecoderChunkOutcome {
    /// Decoded PCM chunk.
    Chunk(Box<AudioChunk>),
    /// Decoder is alive but produced no chunk this call. See
    /// [`PendingReason`] for the precise cause.
    Pending(PendingReason),
    /// Natural end of stream — no more chunks will be produced.
    Eof,
}

impl TryFrom<DecoderChunkOutcome> for AudioChunk {
    type Error = DecoderChunkOutcome;

    fn try_from(outcome: DecoderChunkOutcome) -> Result<Self, Self::Error> {
        match outcome {
            DecoderChunkOutcome::Chunk(chunk) => Ok(*chunk),
            other => Err(other),
        }
    }
}

/// Combined trait for decoder input sources.
///
/// Supertrait combining `Read + Seek + Send + Sync`. Adds typed
/// [`try_read`] returning [`InputReadOutcome`] so decoders never
/// confuse "0 bytes" between EOF and `Pending(...)`.
/// `kithara_stream::Stream` packages its typed status (`VariantChange`,
/// `NotReady`/`Retry`) into `io::Error` payloads via
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
    /// failures. Status conditions (variant change, data not ready)
    /// come back as `Ok(InputReadOutcome::Pending(...))`.
    fn try_read(&mut self, buf: &mut [u8]) -> Result<InputReadOutcome, StreamReadError> {
        match Read::read(self, buf) {
            Ok(0) => Ok(InputReadOutcome::Eof),
            Ok(n) => {
                Ok(NonZeroUsize::new(n).map_or(InputReadOutcome::Eof, InputReadOutcome::Bytes))
            }
            Err(e) => {
                let stream_pending = e
                    .get_ref()
                    .and_then(|src| src.downcast_ref::<StreamPending>())
                    .map(StreamPending::reason);
                let pending = e
                    .get_ref()
                    .and_then(|src| src.downcast_ref::<PendingReason>())
                    .copied();
                let variant = e
                    .get_ref()
                    .and_then(|src| src.downcast_ref::<VariantChangeError>())
                    .is_some();
                let interrupted = e.kind() == ErrorKind::Interrupted;
                match (stream_pending, pending, variant, interrupted) {
                    (Some(reason), _, _, _) | (_, Some(reason), _, _) => {
                        Ok(InputReadOutcome::Pending(reason))
                    }
                    (None, None, true, _) => {
                        Ok(InputReadOutcome::Pending(PendingReason::VariantChange))
                    }
                    (None, None, false, true) => Ok(InputReadOutcome::Pending(
                        PendingReason::NotReady(NotReadyCause::SourcePending),
                    )),
                    (None, None, false, false) => Err(StreamReadError::Source(e)),
                }
            }
        }
    }
}

impl<T: Read + Seek + Send + Sync + ?Sized> DecoderInput for T {}

/// Boxed [`DecoderInput`] alias used by demuxer constructors. The factory
/// dispatch path materialises the input as a `BoxedSource` so concrete
/// demuxers don't have to be generic over the byte source.
pub(crate) type BoxedSource = Box<dyn DecoderInput>;

/// Waiting policy of one front-side preparation or seek settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareWait {
    /// Submit or inspect work without waiting for native input.
    Poll,
    /// Wait for the same operation to finish or be cancelled.
    Wait,
}

/// Whether a decoder has relinquished its input for a source command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputState {
    /// A healthy parser has no operation using the input.
    Idle,
    /// Cancellation is in flight; the source cursor is still owned by the parser.
    Retiring,
    /// The parser has dropped its input and cannot be resumed.
    Retired,
}

/// Trait for runtime-polymorphic audio decoders.
///
/// This trait is used by kithara-audio for dynamic dispatch when the
/// decoder type is determined at runtime (e.g., based on media info).
#[kithara::mock(api = DecoderMock)]
pub trait Decoder: Send + 'static {
    /// Construction-time PCM specification used to configure PCM blending.
    ///
    /// The default exposes the same output specification already returned by
    /// [`Self::spec`].
    fn blender_profile(&self) -> BlenderProfile {
        BlenderProfile::new(self.spec())
    }

    /// Default leading-silence frame count for `codec` when no
    /// container-/encoder-level gapless metadata is available.
    ///
    /// Default implementation returns the codec's encoder-side
    /// priming ([`AudioCodec::encoder_priming_frames`]) — every
    /// decoder inherits it for free. Concrete decoders override only
    /// when they add their own algorithmic delay on top
    /// ([`crate::codec::FrameCodec::decoder_algo_delay`] — currently
    /// Symphonia `mpa` adds 529 for MP3).
    ///
    /// Used by `kithara_audio::pipeline::gapless::resolve_codec_priming`
    /// for the [`crate::GaplessMode::CodecPriming`] fallback path.
    fn default_priming_frames(&self, codec: AudioCodec) -> u64 {
        AudioCodec::encoder_priming_frames(codec)
    }

    /// Original physical PCM position and its content-comparison basis.
    ///
    /// Capture both at the original head, before read-ahead can change the live
    /// gap. The basis removes only the gap already reflected in that head's
    /// position, independent of later PTS jumps and codec gap modelling.
    /// Wrappers retain the exact basis while projecting the physical position.
    /// The record survives seeks; sought output cannot establish an unknown one.
    fn initial_pcm_origin(&self) -> Option<PcmOrigin> {
        None
    }

    /// Raw PCM coordinate of track-media zero, independent of gapless policy and seeks.
    fn media_origin(&self) -> Option<Duration> {
        None
    }

    /// Get total duration from track metadata.
    ///
    /// Returns `None` if duration cannot be determined.
    fn duration(&self) -> Option<crate::DecoderDuration>;

    /// Publish reader-hook events queued during decode.
    ///
    /// `next_chunk` / `seek` resolve hook events on the worker's
    /// forbid-blocking decode core but defer the bus publish into a
    /// lock-free ring. The audio worker drains that ring from its unchecked
    /// shell, once per pass, by calling this. Default no-op for decoders
    /// that carry no reader hooks.
    fn flush_reader_signals(&mut self) {}

    /// Snapshot the existing decoder facts needed to configure gapless trim.
    ///
    /// The default is deliberately an adapter over the established
    /// `spec`/`track_info`/`default_priming_frames` contract, so adding the
    /// profile does not change playback behaviour.
    fn gapless_profile(&self, codec: Option<AudioCodec>) -> GaplessProfile {
        let track_info = self.track_info();
        GaplessProfile::new(
            self.spec(),
            track_info.gapless,
            track_info.gapless_tail,
            codec.map_or(0, |codec| self.default_priming_frames(codec)),
        )
    }

    /// Get track metadata (title, artist, album, artwork).
    ///
    /// Returns default metadata if not available.
    fn metadata(&self) -> TrackMetadata {
        TrackMetadata::default()
    }

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

    /// Decode with storage prepared by [`Self::prepare_next_chunk`].
    ///
    /// # Errors
    /// Returns a preparation or decoding error.
    fn next_chunk_prepared(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.next_chunk()
    }

    /// Whether prepared consumption can deliver PCM, EOF, or an error without more input.
    ///
    /// A provisional Pending outcome is not ready. This observation performs no work and
    /// leaves the retained result owned by the decoder until consumption or a seek.
    fn has_prepared_result(&self) -> bool {
        false
    }

    /// Prepare input and storage before entering a checked decode quantum.
    ///
    /// Completed PCM, EOF, and errors remain unchanged until the following decode call
    /// consumes them. Pending is provisional and may be refreshed by later preparation.
    fn prepare_next_chunk(&mut self) {}

    /// Prepare with an explicit waiting policy for this call.
    fn prepare_next_chunk_with_wait(&mut self, _wait: PrepareWait) {
        self.prepare_next_chunk();
    }

    /// Relinquish an in-flight input operation before another owner changes its cursor.
    fn release_input(&mut self) -> InputState {
        InputState::Idle
    }

    /// Seek to a time position.
    ///
    /// On success returns [`DecoderSeekOutcome::Landed`] with the
    /// authoritative landed position (often a granule boundary near
    /// the requested target — never assume `landed_at == pos`), or
    /// [`DecoderSeekOutcome::PastEof`] when the target is beyond the
    /// decoder's known duration. [`DecoderSeekOutcome::Pending`] carries no landing
    /// coordinate; use [`Self::prepare_seek`] to continue that operation.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError::SeekFailed`] if seeking is not supported
    /// or the position is invalid for reasons other than past-EOF.
    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome>;

    /// Seek with an explicit front-side waiting policy.
    ///
    /// # Errors
    /// Returns the errors described by [`Self::seek`].
    fn seek_with_wait(
        &mut self,
        pos: Duration,
        _wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek(pos)
    }

    /// Continue an unresolved seek without starting it again.
    ///
    /// `None` means no unresolved seek. A terminal outcome settles the operation once;
    /// later calls return `None`. Ordinary PCM preparation does not settle a seek.
    ///
    /// # Errors
    /// Returns source or decoder failures from the same seek operation.
    fn prepare_seek(&mut self) -> DecodeResult<Option<DecoderSeekOutcome>> {
        Ok(None)
    }

    /// Settle the same pending seek with an explicit waiting policy.
    ///
    /// # Errors
    /// Returns the errors described by [`Self::prepare_seek`].
    fn prepare_seek_with_wait(
        &mut self,
        _wait: PrepareWait,
    ) -> DecodeResult<Option<DecoderSeekOutcome>> {
        self.prepare_seek()
    }

    /// Seeks while retaining the native sample at or before `pos` for later rendering.
    ///
    /// Rate adapters forward this operation before converting PCM. The ordinary
    /// seek operation retains its nearest-sample behavior.
    ///
    /// # Errors
    /// Returns an error when native-prefix seeking is unsupported or repositioning fails.
    fn seek_prefix(&mut self, _pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Err(crate::DecodeError::SeekFailed {
            detail: "native-prefix seeking is unsupported",
        })
    }

    /// Seek a native prefix with an explicit front-side waiting policy.
    ///
    /// # Errors
    /// Returns the errors described by [`Self::seek_prefix`].
    fn seek_prefix_with_wait(
        &mut self,
        pos: Duration,
        _wait: PrepareWait,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_prefix(pos)
    }

    /// Get the decoded audio output specification.
    fn spec(&self) -> AudioSpec;

    /// Number of frames between a packet's timestamp and the audio this decoder has
    /// actually produced for it. Live: it settles as the decoder works
    /// through its head strip, so a caller that cached it at construction
    /// would hold the pre-strip value forever.
    fn timeline_gap_frames(&self) -> u64 {
        0
    }

    /// Decoder-owned playback contract — currently the captured
    /// [`crate::GaplessInfo`] (encoder priming + trailing padding in
    /// decoded frames) when `DecoderConfig.gapless = true` and the codec
    /// reports it. Default implementation returns the empty contract,
    /// so most decoders inherit the no-trim behaviour.
    ///
    /// The audio-pipeline gapless stage reads this once per track and
    /// constructs a [`crate::GaplessTrimmer`] when the contract is
    /// non-empty. Returned by-value (clone) so callers don't pin a
    /// borrow on `&self` across `next_chunk`/`seek`.
    fn track_info(&self) -> crate::DecoderTrackInfo {
        crate::DecoderTrackInfo::default()
    }

    /// Update the byte length reported to the underlying media source.
    ///
    /// For HLS streams, the total length becomes known after metadata
    /// calculation. Call this before seeking so the decoder can compute
    /// correct seek deltas.
    fn update_byte_len(&self, len: u64);
}
