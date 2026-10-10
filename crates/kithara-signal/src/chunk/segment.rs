/// Identity of a decoded lane segment.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SegmentId(u64);

impl SegmentId {
    /// The initial segment of a loaded track.
    pub const FIRST: Self = Self(0);

    /// Returns the segment number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next segment, saturating at the largest segment number.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::SegmentId;

    #[kithara::test]
    fn segment_ids_order_by_opening() {
        let first = SegmentId::FIRST;
        let second = first.next();
        let third = second.next();

        assert_eq!(first, SegmentId::default());
        assert_eq!(first.get(), 0);
        assert_eq!(second.get(), 1);
        assert_eq!(third.get(), 2);
        assert!(first < second);
        assert!(second < third);
    }

    #[kithara::test]
    fn segment_id_next_saturates() {
        let last = SegmentId(u64::MAX - 1).next();

        assert_eq!(last.get(), u64::MAX);
        assert_eq!(last.next(), last);
    }
}
