#![forbid(unsafe_code)]

use kithara_platform::time::Duration;

pub type VariantIndex = usize;

/// A bounded segment index: guaranteed to address a real segment
/// (`0 <= index < len`) at construction time. Replaces the former
/// `usize` alias so impossible indices are surfaced, not silently
/// clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentIndex(usize);

impl SegmentIndex {
    /// The raw `usize` value, for indexing and external `usize`/`u32`
    /// lookup boundaries.
    pub(crate) fn into_inner(self) -> usize {
        self.0
    }

    /// Construct an in-bounds segment index. Returns `None` when
    /// `idx >= len`, i.e. the index does not address a real segment.
    pub(crate) fn try_new(idx: usize, len: usize) -> Option<Self> {
        (idx < len).then_some(Self(idx))
    }
}

/// Sum `durations[..endpoint]` when `endpoint` is a valid slice endpoint
/// (`endpoint <= durations.len()`), else `None`.
///
/// `endpoint` is the *exclusive end* of a prefix, not a segment index:
/// `endpoint == len` legitimately means "at/after the last segment" and
/// must sum the full slice. Only `endpoint > len` is impossible/corrupt.
/// This replaces the silent `.min(len)` clamp in `Abr::progress`, which
/// masked an out-of-range endpoint as a full-prefix sum.
pub(crate) fn duration_prefix(durations: &[Duration], endpoint: usize) -> Option<Duration> {
    durations
        .get(..endpoint)
        .map(|prefix| prefix.iter().copied().sum())
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::{SegmentIndex, duration_prefix};

    #[kithara::test]
    fn segment_index_try_new_bounds() {
        assert_eq!(
            SegmentIndex::try_new(2, 4).map(SegmentIndex::into_inner),
            Some(2)
        );
        assert_eq!(
            SegmentIndex::try_new(3, 4).map(SegmentIndex::into_inner),
            Some(3)
        );
        // idx == len is not a real segment index.
        assert_eq!(SegmentIndex::try_new(4, 4), None);
        assert_eq!(SegmentIndex::try_new(5, 4), None);
        // empty: no index is addressable.
        assert_eq!(SegmentIndex::try_new(0, 0), None);
    }

    #[kithara::test]
    fn duration_prefix_endpoint_semantics() {
        let durations = [
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
        ];
        assert_eq!(duration_prefix(&durations, 0), Some(Duration::ZERO));
        assert_eq!(duration_prefix(&durations, 1), Some(Duration::from_secs(1)));
        assert_eq!(duration_prefix(&durations, 2), Some(Duration::from_secs(3)));
        // endpoint == len: full sum, Some (the preserved boundary case).
        assert_eq!(duration_prefix(&durations, 3), Some(Duration::from_secs(6)));
        // endpoint > len: impossible, surfaced as None (no silent clamp).
        assert_eq!(duration_prefix(&durations, 4), None);
    }
}
