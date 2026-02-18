#![forbid(unsafe_code)]

//! Coverage tracking for downloaded byte ranges.
//!
//! [`Coverage`] tracks which byte ranges of a resource have been downloaded.
//! [`MemCoverage`] is an in-memory implementation backed by `rangemap::RangeSet`.

use std::ops::Range;

use rangemap::RangeSet;

/// Tracks downloaded byte ranges of a resource.
///
/// Not tied to Downloader — a standalone abstraction.
/// Implementations may store the index in memory ([`MemCoverage`])
/// or persist it to disk alongside the resource.
#[cfg_attr(test, unimock::unimock(api = CoverageMock))]
pub trait Coverage: Send + 'static {
    /// Mark a range as downloaded.
    fn mark(&mut self, range: Range<u64>);

    /// Whether all bytes have been downloaded (requires known `total_size`).
    fn is_complete(&self) -> bool;

    /// Next gap up to `max_size` bytes. `None` if no gaps remain.
    fn next_gap(&self, max_size: u64) -> Option<Range<u64>>;

    /// All gaps.
    fn gaps(&self) -> Vec<Range<u64>>;

    /// Total size (if known).
    fn total_size(&self) -> Option<u64>;

    /// Set total size (may become known later, e.g. from Content-Length).
    fn set_total_size(&mut self, size: u64);
}

/// In-memory coverage tracker backed by [`RangeSet`].
///
/// `RangeSet` automatically merges adjacent and overlapping ranges.
pub struct MemCoverage {
    ranges: RangeSet<u64>,
    total_size: Option<u64>,
}

impl MemCoverage {
    /// Create a new empty coverage tracker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ranges: RangeSet::new(),
            total_size: None,
        }
    }

    /// Create a coverage tracker with known total size.
    #[must_use]
    pub fn with_total_size(total_size: u64) -> Self {
        Self {
            ranges: RangeSet::new(),
            total_size: Some(total_size),
        }
    }
}

impl Default for MemCoverage {
    fn default() -> Self {
        Self::new()
    }
}

impl Coverage for MemCoverage {
    fn mark(&mut self, range: Range<u64>) {
        if !range.is_empty() {
            self.ranges.insert(range);
        }
    }

    fn is_complete(&self) -> bool {
        let Some(total) = self.total_size else {
            return false;
        };
        if total == 0 {
            return true;
        }
        // Check if 0..total is fully covered.
        !self.ranges.gaps(&(0..total)).any(|_| true)
    }

    fn next_gap(&self, max_size: u64) -> Option<Range<u64>> {
        let total = self.total_size?;
        if total == 0 {
            return None;
        }
        let gap = self.ranges.gaps(&(0..total)).next()?;
        let capped_end = gap.start.saturating_add(max_size).min(gap.end);
        Some(gap.start..capped_end)
    }

    fn gaps(&self) -> Vec<Range<u64>> {
        let Some(total) = self.total_size else {
            return Vec::new();
        };
        if total == 0 {
            return Vec::new();
        }
        self.ranges.gaps(&(0..total)).collect()
    }

    fn total_size(&self) -> Option<u64> {
        self.total_size
    }

    fn set_total_size(&mut self, size: u64) {
        self.total_size = Some(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_and_merge_adjacent() {
        let mut c = MemCoverage::with_total_size(100);
        c.mark(0..50);
        c.mark(50..100);
        assert!(c.is_complete());
    }

    #[test]
    fn mark_and_merge_overlapping() {
        let mut c = MemCoverage::with_total_size(100);
        c.mark(0..60);
        c.mark(40..100);
        assert!(c.is_complete());
    }

    #[test]
    fn gap_detection() {
        let mut c = MemCoverage::with_total_size(100);
        c.mark(0..30);
        c.mark(70..100);

        let gaps = c.gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0], 30..70);
    }

    #[test]
    fn is_complete_with_known_total() {
        let mut c = MemCoverage::with_total_size(100);
        assert!(!c.is_complete());

        c.mark(0..100);
        assert!(c.is_complete());
    }

    #[test]
    fn is_complete_without_total_always_false() {
        let mut c = MemCoverage::new();
        c.mark(0..100);
        assert!(!c.is_complete());
    }

    #[test]
    fn next_gap_with_max_size() {
        let mut c = MemCoverage::with_total_size(1000);
        c.mark(0..100);
        c.mark(500..1000);

        // Gap is 100..500 (400 bytes), but max_size=200 caps it.
        let gap = c.next_gap(200).unwrap();
        assert_eq!(gap, 100..300);
    }

    #[test]
    fn next_gap_uncapped() {
        let mut c = MemCoverage::with_total_size(1000);
        c.mark(0..100);
        c.mark(500..1000);

        let gap = c.next_gap(u64::MAX).unwrap();
        assert_eq!(gap, 100..500);
    }

    #[test]
    fn next_gap_none_when_complete() {
        let mut c = MemCoverage::with_total_size(100);
        c.mark(0..100);
        assert!(c.next_gap(100).is_none());
    }

    #[test]
    fn set_total_size_after_mark() {
        let mut c = MemCoverage::new();
        c.mark(0..50);
        assert!(!c.is_complete());

        c.set_total_size(50);
        assert!(c.is_complete());
    }

    #[test]
    fn empty_range_ignored() {
        let mut c = MemCoverage::with_total_size(100);
        c.mark(50..50); // empty
        assert!(!c.is_complete());
        assert_eq!(c.gaps().len(), 1);
        assert_eq!(c.gaps()[0], 0..100);
    }

    #[test]
    fn zero_total_is_complete() {
        let c = MemCoverage::with_total_size(0);
        assert!(c.is_complete());
    }

    #[test]
    fn multiple_gaps() {
        let mut c = MemCoverage::with_total_size(100);
        c.mark(0..20);
        c.mark(40..60);
        c.mark(80..100);

        let gaps = c.gaps();
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0], 20..40);
        assert_eq!(gaps[1], 60..80);
    }

    #[test]
    fn gaps_without_total_returns_empty() {
        let mut c = MemCoverage::new();
        c.mark(0..50);
        assert!(c.gaps().is_empty());
    }

    #[test]
    fn next_gap_without_total_returns_none() {
        let mut c = MemCoverage::new();
        c.mark(0..50);
        assert!(c.next_gap(100).is_none());
    }

    #[test]
    fn coverage_mock_api_is_generated() {
        let _ = CoverageMock::mark;
    }
}
