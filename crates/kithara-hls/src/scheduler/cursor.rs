#![forbid(unsafe_code)]

//! Download cursor — tracks the next segment index the HLS scheduler
//! should fetch in the current variant.
//!
//! The cursor carries a `floor` (the lowest segment index the scheduler
//! is still responsible for in this download epoch) and a `next` pointer
//! (the next segment to fetch). `next` is monotonically non-decreasing
//! within an epoch and is clamped to `>= floor` on rewind.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DownloadCursor<I> {
    floor: I,
    next: I,
}

impl<I: Copy + Ord> DownloadCursor<I> {
    /// Cursor starting at `start`, with both floor and next set to it.
    #[must_use]
    pub(crate) fn fill(start: I) -> Self {
        Self {
            floor: start,
            next: start,
        }
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
    pub(crate) fn fill_floor(&self) -> I {
        self.floor
    }

    #[must_use]
    pub(crate) fn fill_next(&self) -> I {
        self.next
    }

    pub(crate) fn reset_fill(&mut self, target: I) {
        *self = Self::fill(target);
    }

    pub(crate) fn reopen_fill(&mut self, floor: I, next: I) {
        *self = Self::fill_from(floor, next);
    }

    pub(crate) fn advance_fill_to(&mut self, next: I) {
        if next > self.next {
            self.next = next;
        }
    }

    pub(crate) fn rewind_fill_to(&mut self, next: I) {
        self.next = self.floor.max(next);
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
