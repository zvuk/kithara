#![forbid(unsafe_code)]

/// Segment identification for PCM chunk tracking.
///
/// Used by `SegmentBlender` to detect overlapping segments
/// from different variants during ABR switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSegmentMeta {
    /// Segment index within the playlist.
    pub segment_index: u32,
    /// Variant index (quality level).
    pub variant_index: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_segment_meta_equality() {
        let a = SourceSegmentMeta {
            segment_index: 5,
            variant_index: 0,
        };
        let b = SourceSegmentMeta {
            segment_index: 5,
            variant_index: 0,
        };
        let c = SourceSegmentMeta {
            segment_index: 5,
            variant_index: 1,
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_source_segment_meta_copy() {
        let a = SourceSegmentMeta {
            segment_index: 3,
            variant_index: 2,
        };
        let b = a;
        assert_eq!(a, b);
    }
}
