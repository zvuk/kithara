#![forbid(unsafe_code)]

//! HLS stream context for lock-free segment/variant access.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara_platform::Mutex;
use kithara_stream::{StreamContext, Timeline};

use crate::stream_index::StreamIndex;

/// `StreamContext` for HLS segmented sources.
pub struct HlsStreamContext {
    segments: Arc<Mutex<StreamIndex>>,
    timeline: Timeline,
    variant_index: Arc<AtomicUsize>,
}

impl HlsStreamContext {
    pub fn new(
        timeline: Timeline,
        segments: Arc<Mutex<StreamIndex>>,
        variant_index: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            segments,
            timeline,
            variant_index,
        }
    }
}

impl StreamContext for HlsStreamContext {
    fn byte_offset(&self) -> u64 {
        self.timeline.byte_position()
    }

    #[expect(clippy::cast_possible_truncation, reason = "segment index fits in u32")]
    fn segment_index(&self) -> Option<u32> {
        let offset = self.timeline.segment_position();
        let segments = self.segments.lock_sync();
        segments
            .find_at_offset(offset)
            .map(|seg_ref| seg_ref.segment_index as u32)
    }

    fn variant_index(&self) -> Option<usize> {
        let offset = self.timeline.segment_position();
        let segments = self.segments.lock_sync();
        let result = segments
            .find_at_offset(offset)
            .map(|seg_ref| seg_ref.variant);
        drop(segments);
        result.or_else(|| Some(self.variant_index.load(Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;
    use crate::stream_index::SegmentData;

    #[kithara::test]
    fn test_hls_stream_context_reads_atomics() {
        let timeline = Timeline::new();
        timeline.set_byte_position(1000);
        // Create StreamIndex with 4 variants and 20 segments
        let segments = Arc::new(Mutex::new(StreamIndex::new(4, 20)));
        // Set variant_map so segment 5 belongs to variant 2
        segments.lock_sync().set_layout_variant(2);
        segments.lock_sync().commit_segment(
            2,
            5,
            SegmentData {
                init_len: 0,
                media_len: 200,
                init_url: None,
                media_url: Url::parse("https://example.com/seg.m4s").unwrap(),
            },
        );
        let variant = Arc::new(AtomicUsize::new(2));

        let ctx = HlsStreamContext::new(
            timeline.clone(),
            Arc::clone(&segments),
            Arc::clone(&variant),
        );

        assert_eq!(ctx.byte_offset(), 1000);
        // segment_index uses segment_position (set before byte_position advances)
        timeline.set_segment_position(100);
        assert_eq!(ctx.segment_index(), Some(5));
        assert_eq!(ctx.variant_index(), Some(2));

        // Atomics update: add another segment at variant 3
        segments.lock_sync().set_layout_variant(3);
        segments.lock_sync().commit_segment(
            3,
            10,
            SegmentData {
                init_len: 0,
                media_len: 200,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-2.m4s").unwrap(),
            },
        );
        variant.store(3, Ordering::Relaxed);

        // Variant 3 segment 10 covers byte range 0..200
        timeline.set_segment_position(100);
        assert_eq!(ctx.segment_index(), Some(10));
        assert_eq!(ctx.variant_index(), Some(3));
    }

    #[kithara::test]
    fn segment_index_uses_segment_position_not_byte_position() {
        let timeline = Timeline::new();
        let segments = Arc::new(Mutex::new(StreamIndex::new(1, 10)));
        segments.lock_sync().commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 200,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0.m4s").unwrap(),
            },
        );
        let variant = Arc::new(AtomicUsize::new(0));
        let ctx = HlsStreamContext::new(
            timeline.clone(),
            Arc::clone(&segments),
            Arc::clone(&variant),
        );

        // byte_position has advanced past segment 0's end (as Stream::read does),
        // but segment_position still points inside segment 0
        timeline.set_byte_position(200);
        timeline.set_segment_position(150);
        assert_eq!(ctx.segment_index(), Some(0));
        assert_eq!(ctx.variant_index(), Some(0));
    }
}
