// unimock macro generates code triggering ignored_unit_patterns
#![allow(clippy::ignored_unit_patterns)]

//! Audio pipeline traits.

use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

// Re-export for convenience
pub use kithara_decode::{DecodeError, DecodeResult};
use kithara_decode::{PcmChunk, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::tokio as platform_tokio;
use platform_tokio::sync::Notify;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::ServiceClass;

/// Reason a [`ReadOutcome::Pending`] / [`ChunkOutcome::Pending`] was
/// returned — i.e. why the reader did not advance this call. Each
/// variant maps to a distinct caller action; there is no overlap and
/// no string-matching required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PendingReason {
    /// Producer ringbuf is empty: the consumer has caught up to the
    /// producer's most recent chunk and is waiting for the next one
    /// (mid-stream async pause, post-seek refill).
    Buffering,
    /// A seek was issued; the consumer is waiting for the producer to
    /// acknowledge the new epoch and deliver post-seek frames. Old
    /// pre-seek frames have been drained.
    SeekInProgress,
    /// Upstream stream-layer surfaced a pending status (network stall,
    /// retry, source-level backpressure). The reader will progress
    /// once the stream resumes.
    StreamBackpressure,
}

/// Audio processing effect in the chain (transforms PCM chunks).
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = AudioEffectMock))]
pub trait AudioEffect: Send + 'static {
    /// Flush remaining buffered data (called at end of stream).
    fn flush(&mut self) -> Option<PcmChunk>;

    /// Process a PCM chunk, returning transformed output.
    ///
    /// Returns `None` if the effect is accumulating data (not enough for output yet).
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk>;

    /// Reset internal state (called after seek).
    fn reset(&mut self);
}

/// Result of a PCM read.
///
/// Each variant carries distinct caller semantics — the type system
/// guarantees forward progress in `Frames` (via [`NonZeroUsize`]),
/// while non-progress is explicit in `Pending` with a typed
/// [`PendingReason`]. Failures surface as `Err(DecodeError)`, never
/// as an enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// `count` frames were written into the output buffer (`count > 0`
    /// by construction). `position` is the reader's position
    /// **after** the read.
    Frames {
        count: NonZeroUsize,
        position: Duration,
    },
    /// Reader is alive but produced no frames this call. See
    /// [`PendingReason`] for the precise cause and required caller
    /// action. `position` is the reader's current position (it has
    /// not advanced since the last successful read).
    Pending {
        reason: PendingReason,
        position: Duration,
    },
    /// Natural end of stream — the reader played up to `duration()`.
    /// No more frames will be produced. `position` is the final
    /// position (usually `duration()`).
    Eof { position: Duration },
}

/// Result of a seek — either the reader landed at a known position or
/// the target was past the known duration. Failures surface as
/// `Err(DecodeError)`.
///
/// `Landed` carries both the requested `target` and the actual
/// `landed_at`. The two may differ when the underlying decoder
/// snapped to a granule/segment boundary; callers that want to write
/// a "post-seek" position should use `landed_at`, not `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekOutcome {
    /// Seek completed; reader is now parked at `landed_at`.
    Landed {
        target: Duration,
        landed_at: Duration,
    },
    /// Seek target was past the reader's `duration()`. Reader is
    /// parked at the end; the next `read()` / `next_chunk()` call
    /// returns `Eof`.
    PastEof {
        target: Duration,
        duration: Duration,
    },
}

/// Result of `next_chunk` — either a decoded chunk (with embedded
/// spec/timing metadata), a typed non-progress signal, or natural
/// EOF. Failures surface as `Err(DecodeError)`.
#[derive(Debug)]
pub enum ChunkOutcome {
    /// Next decoded chunk.
    Chunk(PcmChunk),
    /// Reader is alive but has no chunk ready this tick. See
    /// [`PendingReason`] for the precise cause; callers may sleep,
    /// yield, or retry depending on the reason.
    Pending {
        reason: PendingReason,
        position: Duration,
    },
    /// Natural end of stream. `position` is the reader's final position.
    Eof { position: Duration },
}

/// Primary PCM interface for reading decoded audio.
///
/// **Terminal-state contract.** Three failure-mode-agnostic outcomes
/// are distinguishable by the caller:
///
/// - `Ok(ReadOutcome::Frames { .. })` — reader is alive, produced N
///   frames (possibly 0 if waiting for data).
/// - `Ok(ReadOutcome::Eof { .. })` — natural end of stream; no more
///   frames will ever come.
/// - `Err(DecodeError)` — decoder or channel failure. The reader may
///   or may not recover; callers that finalise tracks MUST NOT treat
///   this as EOF.
///
/// **Usage pattern:**
/// ```ignore
/// // Async preload before audio callback
/// resource.preload()?;
///
/// // In audio callback (non-blocking after preload)
/// match resource.read_planar(&mut buffers)? {
///     ReadOutcome::Frames { count, .. } if count > 0 => play_samples(count),
///     ReadOutcome::Frames { .. } => { /* silence this tick */ }
///     ReadOutcome::Eof { .. } => finalise_track(),
/// }
/// ```
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = PcmReaderMock))]
pub trait PcmReader: Send {
    /// Runtime ABR handle for the underlying stream.
    ///
    /// Adaptive readers (HLS) return `Some(handle)` so the queue/FFI can
    /// drive `set_mode` / `set_max_bandwidth_bps` mid-playback. Default
    /// `None` for non-adaptive readers (file, test fixtures).
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        None
    }

    /// Get total duration (if known).
    fn duration(&self) -> Option<Duration>;

    /// Access the unified event bus for subscribing to all pipeline events.
    fn event_bus(&self) -> &EventBus;

    /// Get track metadata.
    fn metadata(&self) -> &TrackMetadata;

    /// Read the next decoded chunk with full metadata.
    ///
    /// Returns [`ChunkOutcome::Chunk`] or [`ChunkOutcome::Eof`].
    /// Decoder / channel failures surface as `Err(DecodeError)`.
    /// Discards any partially-consumed chunk from previous
    /// [`PcmReader::read`] calls.
    ///
    /// Default implementation reports immediate natural EOF — readers
    /// without chunk-level support shouldn't be polled this way.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` for terminal producer failures, same
    /// semantics as [`Self::read`].
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        Ok(ChunkOutcome::Eof {
            position: self.position(),
        })
    }

    /// Get current playback position.
    fn position(&self) -> Duration;

    /// Preload initial chunks into internal buffers.
    ///
    /// After calling this, subsequent `read()` / `read_planar()` /
    /// `next_chunk()` return immediately from buffered data without
    /// blocking. `Err(DecodeError)` is reserved for setup failures
    /// (e.g. the producer channel closed during preload). Natural EOF
    /// encountered during preload is **not** surfaced here — the
    /// subsequent `read` / `next_chunk` will return `Eof`.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` only on terminal setup failure
    /// (closed PCM channel, backend error). Successful preload always
    /// returns `Ok(())` even if the stream contains no data.
    fn preload(&mut self) -> Result<(), DecodeError> {
        Ok(())
    }

    /// Get notify for async preload (first chunk available).
    fn preload_notify(&self) -> Option<Arc<Notify>> {
        None
    }

    /// Read interleaved PCM samples.
    ///
    /// After `preload()`, returns immediately from buffered data
    /// without blocking. The returned [`ReadOutcome`] distinguishes
    /// "produced N frames" (including `count == 0` for transient
    /// stalls) from natural EOF. Decoder / channel failures surface as
    /// `Err(DecodeError)`.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` for terminal producer failures:
    /// closed PCM channel, decoder fault, or backend error. The error
    /// is one-way — once returned, subsequent reads continue to fail.
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError>;

    /// Read deinterleaved (planar) PCM samples.
    ///
    /// After `preload()`, returns immediately from buffered data
    /// without blocking. Each slice in `output` corresponds to one
    /// channel. The returned [`ReadOutcome`] has the same semantics as
    /// [`Self::read`]; `count` is frames-per-channel.
    ///
    /// # Errors
    ///
    /// Same as [`Self::read`] — terminal producer failures are surfaced
    /// as `Err(DecodeError)`.
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError>;

    /// Seek to the given position.
    ///
    /// Returns [`SeekOutcome::Landed`] when the reader is now parked
    /// at the requested position, [`SeekOutcome::PastEof`] when the
    /// target was beyond `duration()`. Seek failures (stream I/O,
    /// decoder recreate) surface as `Err(DecodeError)`.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` when seek cannot complete: stream I/O
    /// failure, decoder recreate failure, or terminal producer error.
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError>;

    /// Set the target sample rate of the audio host.
    ///
    /// Used for dynamic updates when the host sample rate changes at runtime.
    fn set_host_sample_rate(&self, _sample_rate: NonZeroU32) {}

    /// Set the playback rate for timeline scaling.
    ///
    /// Rate > 1.0 speeds up playback (position advances faster).
    /// Rate < 1.0 slows down playback (position advances slower).
    /// The actual pitch-shifting is done by the resampler.
    fn set_playback_rate(&self, _rate: f32) {}

    /// Update the scheduling priority hint for the shared worker.
    ///
    /// Maps track playback state to worker priority: `Audible` tracks
    /// are decoded first, then `Warm`, then `Idle`.
    fn set_service_class(&self, _class: ServiceClass) {}

    /// Get the current PCM specification.
    fn spec(&self) -> PcmSpec;
}
