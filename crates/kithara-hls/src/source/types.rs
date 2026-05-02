use kithara_platform::time::Duration;
use url::Url;

use crate::{
    ids::{SegmentIndex, VariantIndex},
    stream_index::SegmentRef,
};

/// Condvar sleep per spin in `wait_range`. Kept short so the audio worker
/// can round-robin between tracks without one slow source starving others.
/// Previous value 50ms caused audible glitches during multi-track mixing.
pub(super) const WAIT_RANGE_SLEEP_MS: u64 = 2;
pub(super) const WAIT_RANGE_HANG_TIMEOUT_FLOOR: Duration = Duration::from_secs(5);

/// Seek classification: whether the committed byte layout is preserved or reset.
#[derive(Debug)]
pub(crate) enum SeekLayout {
    /// Same variant — keep `StreamIndex`, byte layout unchanged.
    Preserve,
    /// Different variant — reset `StreamIndex`, rebuild layout.
    Reset,
}

/// Snapshot of segment data needed for reading, copied out of the lock.
pub(crate) struct ReadSegment {
    pub(crate) init_url: Option<Url>,
    pub(crate) segment_index: SegmentIndex,
    pub(crate) media_url: Url,
    pub(crate) variant: VariantIndex,
    pub(crate) byte_offset: u64,
    pub(crate) init_len: u64,
    pub(crate) media_len: u64,
}

impl ReadSegment {
    /// Snapshot a committed `SegmentRef`. Returns `None` for reserved
    /// slots (segment has a byte-map entry but no data yet).
    pub(crate) fn try_from_ref(seg_ref: &SegmentRef<'_>) -> Option<Self> {
        let data = seg_ref.data?;
        Some(Self {
            variant: seg_ref.variant,
            segment_index: seg_ref.segment_index,
            byte_offset: seg_ref.byte_offset,
            init_len: data.init_len,
            media_len: data.media_len,
            init_url: data.init_url.clone(),
            media_url: data.media_url.clone(),
        })
    }
}
