use kithara_platform::time::Duration;
use url::Url;

use crate::{
    ids::{SegmentIndex, VariantIndex},
    stream_index::SegmentRef,
};

pub(super) const WAIT_RANGE_MAX_METADATA_MISS_SPINS: usize = 25;
/// Condvar sleep per spin in `wait_range`. Kept short so the audio worker
/// can round-robin between tracks without one slow source starving others.
/// Previous value 50ms caused audible glitches during multi-track mixing.
pub(super) const WAIT_RANGE_SLEEP_MS: u64 = 2;
pub(super) const WAIT_RANGE_HANG_TIMEOUT_FLOOR: Duration = Duration::from_secs(5);

/// Seek classification: whether the committed byte layout is preserved or reset.
pub(crate) enum SeekLayout {
    /// Same variant — keep `StreamIndex`, byte layout unchanged.
    Preserve,
    /// Different variant — reset `StreamIndex`, rebuild layout.
    Reset,
}

#[derive(Clone, Copy)]
pub(crate) enum ResolvedSegment {
    Committed(SegmentIndex),
    Layout(SegmentIndex),
    Fallback(SegmentIndex),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetadataMissReason {
    VariantOutOfRange,
    UnresolvedOffset,
}

impl MetadataMissReason {
    pub(crate) const fn label(self) -> Option<&'static str> {
        match self {
            Self::VariantOutOfRange => Some("variant_out_of_range"),
            Self::UnresolvedOffset => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemandRequestOutcome {
    Requested {
        queued: bool,
    },
    TransientGap,
    MetadataMiss {
        variant: VariantIndex,
        reason: MetadataMissReason,
    },
}

/// Snapshot of segment data needed for reading, copied out of the lock.
pub(crate) struct ReadSegment {
    pub(crate) variant: VariantIndex,
    pub(crate) segment_index: SegmentIndex,
    pub(crate) byte_offset: u64,
    pub(crate) init_len: u64,
    pub(crate) media_len: u64,
    pub(crate) init_url: Option<Url>,
    pub(crate) media_url: Url,
}

impl ReadSegment {
    /// Create from a `SegmentRef` (borrows from the lock — must copy).
    pub(crate) fn from_ref(seg_ref: &SegmentRef<'_>) -> Self {
        Self {
            variant: seg_ref.variant,
            segment_index: seg_ref.segment_index,
            byte_offset: seg_ref.byte_offset,
            init_len: seg_ref.data.init_len,
            media_len: seg_ref.data.media_len,
            init_url: seg_ref.data.init_url.clone(),
            media_url: seg_ref.data.media_url.clone(),
        }
    }

    pub(crate) fn end_offset(&self) -> u64 {
        self.byte_offset + self.init_len + self.media_len
    }
}
