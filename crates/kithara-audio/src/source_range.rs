use std::{ops::Range, sync::atomic::Ordering};

use kithara_decode::DecodeError;
use portable_atomic::AtomicU8;

/// An exact frame index on the decoded, gapless-normalized source axis.
#[derive(Clone, Copy, Debug, Default, derive_more::From, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceFrameIndex(u64);

impl SourceFrameIndex {
    /// Returns the integer source-frame index.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<usize> for SourceFrameIndex {
    type Error = SourceRangeError;

    fn try_from(index: usize) -> Result<Self, Self::Error> {
        u64::try_from(index)
            .map(Self)
            .map_err(|_| SourceRangeError::ArithmeticOverflow)
    }
}

/// A checked, non-empty half-open range on the decoded source axis.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct SourceRange {
    start: SourceFrameIndex,
    end: SourceFrameIndex,
}

impl SourceRange {
    /// Returns the inclusive start frame.
    #[must_use]
    pub const fn start(self) -> SourceFrameIndex {
        self.start
    }

    /// Returns the exclusive end frame.
    #[must_use]
    pub const fn end(self) -> SourceFrameIndex {
        self.end
    }

    /// Returns the number of frames in the range.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.end.0 - self.start.0
    }

    /// Returns false because construction rejects empty ranges.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }
}

impl TryFrom<Range<u64>> for SourceRange {
    type Error = SourceRangeError;

    fn try_from(range: Range<u64>) -> Result<Self, Self::Error> {
        if range.start >= range.end {
            return Err(SourceRangeError::EmptyOrInverted {
                start: range.start,
                end: range.end,
            });
        }
        Ok(Self {
            start: range.start.into(),
            end: range.end.into(),
        })
    }
}

/// Opaque proof that a bounded read belongs to the current canonical seek epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct SourceRangeRequest {
    range: SourceRange,
    seek_epoch: u64,
}

impl SourceRangeRequest {
    pub(crate) const fn new(range: SourceRange, seek_epoch: u64) -> Self {
        Self { range, seek_epoch }
    }

    /// Returns the requested source range.
    #[must_use]
    pub const fn range(self) -> SourceRange {
        self.range
    }

    /// Returns the canonical seek epoch that owns this request.
    #[must_use]
    pub const fn seek_epoch(self) -> u64 {
        self.seek_epoch
    }
}

/// Result of polling one canonical bounded source read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRangeReadOutcome {
    /// Every requested frame was copied into the caller's fixed buffer.
    Ready { frames: usize },
    /// The worker has not produced every requested frame yet.
    Pending,
    /// The decoder reached natural EOF before the range was complete.
    Eof,
}

/// Errors raised by canonical bounded source reads.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceRangeError {
    /// The requested range contains no frames or ends before it starts.
    #[error("source range must be non-empty and ascending: {start}..{end}")]
    EmptyOrInverted { start: u64, end: u64 },
    /// Frame or sample arithmetic exceeded the supported integer domain.
    #[error("source range arithmetic overflowed")]
    ArithmeticOverflow,
    /// The reader does not provide canonical bounded source reads.
    #[error("reader does not support canonical bounded source reads")]
    Unsupported,
    /// A newer seek superseded this range request.
    #[error("source range request belongs to a stale seek epoch")]
    StaleRequest,
    /// The supplied request is not the active range owned by this reader.
    #[error("source range request is not active on this reader")]
    RequestMismatch,
    /// The destination length does not match the requested frame range.
    #[error("source range output has the wrong size: expected {expected}, got {actual}")]
    OutputSizeMismatch { expected: usize, actual: usize },
    /// A decoded chunk's metadata does not match its interleaved sample buffer.
    #[error("decoded chunk has the wrong sample count: expected {expected}, got {actual}")]
    ChunkSampleCountMismatch { expected: usize, actual: usize },
    /// A decoded chunk changed format inside one prepared range.
    #[error("decoded source format changed inside a bounded read")]
    SpecMismatch,
    /// The decoder skipped a source interval required by the request.
    #[error("decoded source range has a gap: expected frame {expected}, got {actual}")]
    Discontinuous { expected: u64, actual: u64 },
    /// The decoder or canonical PCM channel failed.
    #[error("decoded source failed before the requested range became available")]
    SourceFailed,
    /// The requested start lies at or past the known end of the source.
    #[error("source range starts at or past end of source")]
    PastEof,
    /// The canonical seek operation failed.
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AudioReadMode {
    #[default]
    Linear = 0,
    Bounded = 1,
}

#[derive(Debug, Default)]
pub(crate) struct AtomicAudioReadMode(AtomicU8);

impl AtomicAudioReadMode {
    pub(crate) fn load(&self) -> AudioReadMode {
        match self.0.load(Ordering::Acquire) {
            1 => AudioReadMode::Bounded,
            _ => AudioReadMode::Linear,
        }
    }

    pub(crate) fn store(&self, mode: AudioReadMode) {
        self.0.store(mode as u8, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{SourceFrameIndex, SourceRange, SourceRangeError};

    #[kithara::test]
    fn source_range_requires_a_non_empty_ascending_interval() {
        assert!(matches!(
            SourceRange::try_from(10..10),
            Err(SourceRangeError::EmptyOrInverted { start: 10, end: 10 })
        ));
        let inverted = [11, 10];
        assert!(matches!(
            SourceRange::try_from(inverted[0]..inverted[1]),
            Err(SourceRangeError::EmptyOrInverted { start: 11, end: 10 })
        ));

        let range = SourceRange::try_from(10..12).expect("ascending range");
        assert_eq!(range.start(), SourceFrameIndex::from(10));
        assert_eq!(range.end(), SourceFrameIndex::from(12));
        assert_eq!(range.len(), 2);
    }
}
