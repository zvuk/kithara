#![forbid(unsafe_code)]

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadCursor<I> {
    Stream { from: I },
    Fill { floor: I, next: I },
    Complete,
}

impl<I: Copy + Ord> DownloadCursor<I> {
    #[must_use]
    pub fn stream(from: I) -> Self {
        Self::Stream { from }
    }

    #[must_use]
    pub fn fill(start: I) -> Self {
        Self::Fill {
            floor: start,
            next: start,
        }
    }

    #[must_use]
    pub fn fill_from(floor: I, next: I) -> Self {
        Self::Fill {
            floor,
            next: next.max(floor),
        }
    }

    #[must_use]
    pub fn complete() -> Self {
        Self::Complete
    }

    #[must_use]
    pub fn stream_from(&self) -> Option<I> {
        match *self {
            Self::Stream { from } => Some(from),
            Self::Fill { .. } | Self::Complete => None,
        }
    }

    #[must_use]
    pub fn fill_floor(&self) -> Option<I> {
        match *self {
            Self::Fill { floor, next: _ } => Some(floor),
            Self::Stream { .. } | Self::Complete => None,
        }
    }

    #[must_use]
    pub fn fill_next(&self) -> Option<I> {
        match *self {
            Self::Fill { floor: _, next } => Some(next),
            Self::Stream { .. } | Self::Complete => None,
        }
    }

    #[must_use]
    pub fn is_stream(&self) -> bool {
        matches!(self, Self::Stream { .. })
    }

    #[must_use]
    pub fn is_fill(&self) -> bool {
        matches!(self, Self::Fill { .. })
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    pub fn reset_fill(&mut self, target: I) {
        *self = Self::fill(target);
    }

    pub fn reopen_fill(&mut self, floor: I, next: I) {
        *self = Self::fill_from(floor, next);
    }

    pub fn restart_stream(&mut self, from: I) {
        *self = Self::stream(from);
    }

    pub fn mark_complete(&mut self) {
        *self = Self::Complete;
    }

    pub fn advance_fill_to(&mut self, next: I) {
        if let Self::Fill {
            floor: _,
            next: current,
        } = self
            && next > *current
        {
            *current = next;
        }
    }

    pub fn rewind_fill_to(&mut self, next: I) {
        if let Self::Fill {
            floor,
            next: current,
        } = self
        {
            *current = (*floor).max(next);
        }
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::DownloadCursor;

    #[kithara::test]
    fn fill_from_clamps_next_to_floor() {
        let cursor = DownloadCursor::fill_from(10_u64, 5);
        assert_eq!(cursor.fill_floor(), Some(10));
        assert_eq!(cursor.fill_next(), Some(10));
    }

    #[kithara::test]
    fn rewind_fill_never_goes_below_floor() {
        let mut cursor = DownloadCursor::fill_from(10_u64, 20);
        cursor.rewind_fill_to(5);
        assert_eq!(cursor.fill_floor(), Some(10));
        assert_eq!(cursor.fill_next(), Some(10));
    }

    #[kithara::test]
    fn advance_fill_is_monotonic() {
        let mut cursor = DownloadCursor::fill(7_usize);
        cursor.advance_fill_to(9);
        cursor.advance_fill_to(8);
        assert_eq!(cursor.fill_floor(), Some(7));
        assert_eq!(cursor.fill_next(), Some(9));
    }

    #[kithara::test]
    fn restart_stream_replaces_fill_state() {
        let mut cursor = DownloadCursor::fill_from(4_u64, 8);
        cursor.restart_stream(12);
        assert_eq!(cursor.stream_from(), Some(12));
        assert!(cursor.fill_next().is_none());
    }
}
