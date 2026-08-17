use std::num::{NonZeroU32, NonZeroUsize};

use kithara_decode::PcmChunk;
use kithara_platform::time::Duration;

use crate::musical::SessionFrame;

/// Immutable producer endpoint attached to one final PCM block.
///
/// The point describes committed presentation state. It does not prove that
/// the consumer has played the block; [`PresentationAdvance`] carries that
/// proof after the block boundary is crossed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PresentationPoint {
    epoch: u64,
    source_frame: u64,
    generation: u64,
    output_end: u64,
    sample_rate: NonZeroU32,
}

impl PresentationPoint {
    /// Creates one final-output presentation endpoint.
    #[must_use]
    pub const fn new(
        seek_epoch: u64,
        source_frame: u64,
        generation: u64,
        output_end: u64,
        sample_rate: NonZeroU32,
    ) -> Self {
        Self {
            epoch: seek_epoch,
            source_frame,
            generation,
            output_end,
            sample_rate,
        }
    }

    /// Seek epoch that owns this point.
    #[must_use]
    pub const fn seek_epoch(self) -> u64 {
        self.epoch
    }

    /// Source frame presented through this block boundary.
    #[must_use]
    pub const fn source_frame(self) -> u64 {
        self.source_frame
    }

    /// Presentation generation within the seek epoch.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Cumulative output frame ordinal within the presentation generation.
    #[must_use]
    pub const fn output_end(self) -> u64 {
        self.output_end
    }

    /// Sample rate of the source-frame axis.
    #[must_use]
    pub const fn sample_rate(self) -> NonZeroU32 {
        self.sample_rate
    }
}

/// One producer endpoint mapped to the absolute session clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PresentationCursor {
    point: PresentationPoint,
    session_frame: SessionFrame,
}

impl PresentationCursor {
    /// Maps an available producer endpoint to an absolute session frame.
    ///
    #[must_use]
    pub const fn new(point: PresentationPoint, session_frame: SessionFrame) -> Self {
        Self {
            point,
            session_frame,
        }
    }

    /// Producer point mapped by this cursor.
    #[must_use]
    pub const fn point(self) -> PresentationPoint {
        self.point
    }

    delegate::delegate! {
        to self.point {
            /// Seek epoch owning the producer point.
            #[must_use]
            pub const fn seek_epoch(self) -> u64;
            /// Source frame presented through the producer point.
            #[must_use]
            pub const fn source_frame(self) -> u64;
            /// Presentation generation within the seek epoch.
            #[must_use]
            pub const fn generation(self) -> u64;
            /// Cumulative output-frame ordinal within the presentation generation.
            #[must_use]
            pub const fn output_end(self) -> u64;
            /// Sample rate of the source-frame axis.
            #[must_use]
            #[call(sample_rate)]
            pub const fn source_rate(self) -> NonZeroU32;
        }
    }

    /// Absolute host-session frame corresponding to [`Self::point`].
    #[must_use]
    pub const fn session_frame(self) -> SessionFrame {
        self.session_frame
    }
}

/// Exact presentation boundary crossed by the most recent PCM read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PresentationAdvance {
    point: PresentationPoint,
    read_offset_frames: usize,
}

impl PresentationAdvance {
    /// Creates proof that `point` was crossed at a frame offset in one read.
    #[must_use]
    pub const fn new(point: PresentationPoint, read_offset_frames: usize) -> Self {
        Self {
            point,
            read_offset_frames,
        }
    }

    /// Producer point whose final PCM block was consumed.
    #[must_use]
    pub const fn point(self) -> PresentationPoint {
        self.point
    }

    /// Frame offset of that boundary within the PCM read that crossed it.
    #[must_use]
    pub const fn read_offset_frames(self) -> usize {
        self.read_offset_frames
    }
}

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
