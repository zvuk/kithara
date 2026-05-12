#![forbid(unsafe_code)]

use std::collections::HashSet;

use crate::ids::SegmentIndex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DownloadCursor<I> {
    floor: I,
    next: I,
}

/// Per-variant download bookkeeping. One instance per active downloader
/// track. In Phase 1 of the two-cursor refactor exactly one
/// [`VariantDownloadState`] is active at a time; Phase 3 introduces a
/// second instance for the blender period.
#[derive(Default)]
pub(crate) struct VariantDownloadState {
    /// Next-segment / floor pointer for this variant's download track.
    /// `SchedulerRuntime` no longer owns a global cursor; reads/writes
    /// go through the helpers on `HlsScheduler`
    /// (`primary_cursor` / `primary_cursor_mut`) which key into
    /// `active_downloads` by the current `primary_variant`.
    pub(crate) cursor: DownloadCursor<SegmentIndex>,
    /// Media segments whose `FetchCmd` has been emitted and whose
    /// `on_complete` has not yet fired. Keyed by `SegmentIndex` only —
    /// the variant identity is the map key in
    /// `SchedulerRuntime::active_downloads`.
    pub(crate) in_flight: HashSet<SegmentIndex>,
    /// `true` once the init segment for this variant has been observed
    /// as committed (cached commit, or first streaming commit with
    /// non-zero `init_len`).
    pub(crate) init_sent: bool,
}

impl<I: Copy + Ord + std::fmt::Debug> DownloadCursor<I> {
    pub(crate) fn advance_fill_to(&mut self, next: I) {
        if next > self.next {
            self.next = next;
        }
    }

    /// Cursor starting at `start`, with both floor and next set to it.
    #[must_use]
    pub(crate) fn fill(start: I) -> Self {
        Self {
            floor: start,
            next: start,
        }
    }

    #[must_use]
    pub(crate) fn fill_floor(&self) -> I {
        self.floor
    }

    /// Cursor with an explicit floor. `next` is clamped to `>= floor`.
    #[must_use]
    pub(crate) fn fill_from(floor: I, next: I) -> Self {
        Self {
            floor,
            next: next.max(floor),
        }
    }

    #[must_use]
    pub(crate) fn fill_next(&self) -> I {
        self.next
    }

    pub(crate) fn reopen_fill(&mut self, floor: I, next: I) {
        tracing::debug!(from_next = ?self.next, from_floor = ?self.floor, new_floor = ?floor, new_next = ?next, "cursor::reopen_fill");
        *self = Self::fill_from(floor, next);
    }

    pub(crate) fn reset_fill(&mut self, target: I) {
        tracing::debug!(from_next = ?self.next, from_floor = ?self.floor, to = ?target, "cursor::reset_fill");
        *self = Self::fill(target);
    }

    pub(crate) fn rewind_fill_to(&mut self, next: I) {
        let result = self.floor.max(next);
        if result < self.next {
            tracing::debug!(from = ?self.next, to = ?result, floor = ?self.floor, target = ?next, "cursor::rewind_fill_to");
        }
        self.next = result;
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::DownloadCursor;

    #[kithara::test]
    fn fill_from_clamps_next_to_floor() {
        let cursor = DownloadCursor::fill_from(10_u64, 5);
        assert_eq!(cursor.fill_floor(), 10);
        assert_eq!(cursor.fill_next(), 10);
    }

    #[kithara::test]
    fn rewind_fill_never_goes_below_floor() {
        let mut cursor = DownloadCursor::fill_from(10_u64, 20);
        cursor.rewind_fill_to(5);
        assert_eq!(cursor.fill_floor(), 10);
        assert_eq!(cursor.fill_next(), 10);
    }

    #[kithara::test]
    fn advance_fill_is_monotonic() {
        let mut cursor = DownloadCursor::fill(7_usize);
        cursor.advance_fill_to(9);
        cursor.advance_fill_to(8);
        assert_eq!(cursor.fill_floor(), 7);
        assert_eq!(cursor.fill_next(), 9);
    }
}
