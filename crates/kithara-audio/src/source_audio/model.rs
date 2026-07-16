use std::num::NonZeroU64;

use kithara_bufpool::PcmBuf;
use kithara_decode::PcmSpec;

/// A checked half-open frame range in decoded source coordinates.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct SourceFrameRange {
    start: u64,
    end: u64,
}

impl SourceFrameRange {
    /// Construct `start..end`, rejecting an inverted range.
    ///
    /// # Errors
    ///
    /// Returns [`SourceAudioError::InvertedRange`] when `start` is greater than `end`.
    pub fn new(start: u64, end: u64) -> Result<Self, SourceAudioError> {
        if start > end {
            return Err(SourceAudioError::InvertedRange { start, end });
        }
        Ok(Self { start, end })
    }

    /// Construct a range from its start and frame count.
    ///
    /// # Errors
    ///
    /// Returns [`SourceAudioError::FrameOverflow`] when the end frame cannot be represented.
    pub fn with_len(start: u64, len: u64) -> Result<Self, SourceAudioError> {
        let end = start
            .checked_add(len)
            .ok_or(SourceAudioError::FrameOverflow)?;
        Ok(Self { start, end })
    }

    /// Return the inclusive start frame.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Return the exclusive end frame.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.end
    }

    /// Return the number of frames in the range.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    /// Return whether the range contains no frames.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub(crate) const fn contains_frame(self, frame: u64) -> bool {
        self.start <= frame && frame < self.end
    }

    pub(crate) const fn intersects(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// An opaque request token bound to one reader generation and decode seek epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct SourceAudioDemand {
    pub(crate) lane_id: NonZeroU64,
    pub(crate) generation: u64,
    pub(crate) decode_seek_epoch: u64,
    pub(crate) requested: SourceFrameRange,
    pub(crate) coverage: SourceFrameRange,
}

impl SourceAudioDemand {
    /// Return the decode seek epoch captured by this demand.
    #[must_use]
    pub const fn decode_seek_epoch(self) -> u64 {
        self.decode_seek_epoch
    }
}

/// Result of a nonblocking source-audio read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SourceAudioReadOutcome {
    /// The requested frames were copied into the output.
    Ready { frames: usize },
    /// One or more requested frames are not cached yet.
    Pending,
    /// The source ended before every requested frame became available.
    Eof,
}

/// Errors raised by the source-audio sidecar contract.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SourceAudioError {
    /// The requested frame range ends before it starts.
    #[error("source frame range is inverted: {start}..{end}")]
    InvertedRange { start: u64, end: u64 },
    /// Frame-range arithmetic exceeded the supported integer domain.
    #[error("source frame arithmetic overflowed")]
    FrameOverflow,
    /// The requested frame range contains no frames.
    #[error("source audio range must not be empty")]
    EmptyRange,
    /// The source format contains no channels.
    #[error("source audio requires at least one channel")]
    EmptyChannelLayout,
    /// Interleaved sample-count arithmetic overflowed.
    #[error("source audio sample count overflowed")]
    SampleCountOverflow,
    /// A captured window contains a different sample count than its metadata.
    #[error("source audio sample count mismatch: expected {expected}, got {actual}")]
    SampleCountMismatch { expected: usize, actual: usize },
    /// The caller-provided output length does not match the requested range.
    #[error("source audio output has the wrong size: expected {expected}, got {actual}")]
    OutputSizeMismatch { expected: usize, actual: usize },
    /// The decoded source format changed after activation.
    #[error("source audio format does not match the active source")]
    SpecMismatch,
    /// The reader has not been activated.
    #[error("source audio reader is inactive")]
    Inactive,
    /// The demand was created by another reader lane.
    #[error("source audio demand belongs to another reader")]
    ForeignDemand,
    /// A newer demand superseded the supplied demand.
    #[error("source audio demand is stale")]
    StaleDemand,
    /// The read range is not covered by the supplied demand.
    #[error("source audio read range lies outside the active demand coverage")]
    RangeOutsideDemand,
    /// The demand generation counter cannot advance further.
    #[error("source audio demand generation is exhausted")]
    GenerationExhausted,
    /// The command lane cannot accept another command yet.
    #[error("source audio command channel is backpressured")]
    CommandBackpressure,
    /// The captured-data lane cannot accept another window yet.
    #[error("source audio data channel is backpressured")]
    DataBackpressure,
    /// The configured reusable-buffer budget is exhausted.
    #[error("source audio buffer budget is exhausted")]
    BufferBudgetExhausted,
    /// The reusable-buffer bank has not been prepared.
    #[error("source audio buffer bank is not prepared")]
    BufferBankNotPrepared,
    /// The requested connection capacity cannot be represented.
    #[error("source audio connection capacity overflowed")]
    CapacityOverflow,
    /// No additional lane identifier can be allocated.
    #[error("source audio lane identifiers are exhausted")]
    LaneExhausted,
    /// Decoding failed before the requested frames became available.
    #[error("decoded source failed before the requested audio became available")]
    SourceFailed,
}

#[derive(Debug)]
pub(crate) struct SourceAudioWindow {
    range: SourceFrameRange,
    spec: PcmSpec,
    samples: PcmBuf,
    sample_len: usize,
}

impl SourceAudioWindow {
    pub(crate) fn validated(
        range: SourceFrameRange,
        spec: PcmSpec,
        samples: PcmBuf,
        sample_len: usize,
    ) -> Self {
        Self {
            range,
            spec,
            samples,
            sample_len,
        }
    }

    pub(crate) const fn range(&self) -> SourceFrameRange {
        self.range
    }

    pub(crate) const fn spec(&self) -> PcmSpec {
        self.spec
    }

    pub(crate) fn samples(&self) -> &[f32] {
        &self.samples[..self.sample_len]
    }

    pub(crate) fn release_samples(self) -> PcmBuf {
        self.samples
    }
}

pub(crate) enum SourceAudioCommand {
    Activate {
        lane_id: NonZeroU64,
        role: SourceAudioRole,
        spec: PcmSpec,
    },
    Demand(SourceAudioDemand),
}

pub(crate) struct SourceAudioPacket {
    pub(crate) demand: SourceAudioDemand,
    pub(crate) window: SourceAudioWindow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceAudioTerminal {
    Eof,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SourceAudioStatus {
    pub(crate) decode_seek_epoch: u64,
    pub(crate) lane_id: NonZeroU64,
    pub(crate) terminal: SourceAudioTerminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceAudioCaptureOutcome {
    Captured,
    DemandComplete,
    Ignored,
    PreparationPending,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SourceAudioRole {
    #[default]
    Mirror,
    Authoritative,
}

pub(crate) fn sample_count(
    range: SourceFrameRange,
    spec: PcmSpec,
) -> Result<usize, SourceAudioError> {
    let channels = usize::from(spec.channels);
    if channels == 0 {
        return Err(SourceAudioError::EmptyChannelLayout);
    }
    let frames = usize::try_from(range.len()).map_err(|_| SourceAudioError::SampleCountOverflow)?;
    frames
        .checked_mul(channels)
        .ok_or(SourceAudioError::SampleCountOverflow)
}

impl SourceAudioError {
    pub(crate) const fn decode_detail(&self) -> &'static str {
        match self {
            Self::InvertedRange { .. } => "source audio frame range is inverted",
            Self::FrameOverflow => "source audio frame arithmetic overflowed",
            Self::EmptyRange => "source audio range is empty",
            Self::EmptyChannelLayout => "source audio channel layout is empty",
            Self::SampleCountOverflow => "source audio sample count overflowed",
            Self::SampleCountMismatch { .. } => "source audio sample count does not match metadata",
            Self::OutputSizeMismatch { .. } => "source audio output has the wrong size",
            Self::SpecMismatch => "source audio format changed while capture was active",
            Self::Inactive => "source audio reader is inactive",
            Self::ForeignDemand => "source audio demand belongs to another reader",
            Self::StaleDemand => "source audio demand is stale",
            Self::RangeOutsideDemand => "source audio read range lies outside demand coverage",
            Self::GenerationExhausted => "source audio demand generation is exhausted",
            Self::CommandBackpressure => "source audio command channel is backpressured",
            Self::DataBackpressure => "source audio data channel is backpressured",
            Self::BufferBudgetExhausted => "source audio buffer budget is exhausted",
            Self::BufferBankNotPrepared => "source audio buffer bank is not prepared",
            Self::CapacityOverflow => "source audio channel capacity overflowed",
            Self::LaneExhausted => "source audio lane identifiers are exhausted",
            Self::SourceFailed => "decoded source failed before source audio became available",
        }
    }
}
