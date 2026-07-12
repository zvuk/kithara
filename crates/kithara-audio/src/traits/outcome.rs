use std::num::NonZeroUsize;

use kithara_decode::PcmChunk;
use kithara_platform::time::Duration;

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
