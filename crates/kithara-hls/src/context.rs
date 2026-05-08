#![forbid(unsafe_code)]

//! HLS stream context for lock-free segment/variant access.

use std::sync::Arc;

use kithara_abr::AbrState;
use kithara_platform::Mutex;
use kithara_stream::{StreamContext, Timeline};

use crate::stream_index::StreamIndex;

/// `StreamContext` for HLS segmented sources.
pub struct HlsStreamContext {
    abr_state: Arc<AbrState>,
    segments: Arc<Mutex<StreamIndex>>,
    timeline: Timeline,
}

impl HlsStreamContext {
    pub fn new(
        timeline: Timeline,
        segments: Arc<Mutex<StreamIndex>>,
        abr_state: Arc<AbrState>,
    ) -> Self {
        Self {
            abr_state,
            segments,
            timeline,
        }
    }
}

impl StreamContext for HlsStreamContext {
    fn byte_offset(&self) -> u64 {
        self.timeline.byte_position()
    }

    fn segment_index(&self) -> Option<u32> {
        let offset = self.timeline.segment_position();
        let segments = self.segments.lock_sync();
        segments
            .find_at_offset(offset)
            .and_then(|seg_ref| u32::try_from(seg_ref.segment_index).ok())
    }

    fn variant_index(&self) -> Option<usize> {
        let offset = self.timeline.segment_position();
        let segments = self.segments.lock_sync();
        let result = segments
            .find_at_offset(offset)
            .map(|seg_ref| seg_ref.variant);
        drop(segments);
        result.or_else(|| Some(self.abr_state.current_variant_index()))
    }
}

#[cfg(test)]
mod tests {
    use kithara_abr::{AbrDecision, AbrState};
    use kithara_events::{AbrMode, AbrReason};
    use kithara_platform::time::Instant;
    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;
    use crate::stream_index::SegmentData;

    fn fresh_abr(initial_variant: usize) -> Arc<AbrState> {
        Arc::new(AbrState::new(
            Vec::new(),
            AbrMode::Auto(Some(initial_variant)),
        ))
    }

    fn pin_variant(abr: &AbrState, idx: usize) {
        abr.apply(
            &AbrDecision {
                reason: AbrReason::ManualOverride,
                did_change: true,
                target_variant_index: idx,
            },
            Instant::now(),
        );
    }

    #[kithara::test]
    fn test_hls_stream_context_reads_atomics() {
        let timeline = Timeline::new();
        timeline.set_byte_position(1000);
        let segments = Arc::new(Mutex::new(StreamIndex::new(4, 20)));
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
        let abr = fresh_abr(2);

        let ctx = HlsStreamContext::new(timeline.clone(), Arc::clone(&segments), Arc::clone(&abr));

        assert_eq!(ctx.byte_offset(), 1000);
        timeline.set_segment_position(100);
        assert_eq!(ctx.segment_index(), Some(5));
        assert_eq!(ctx.variant_index(), Some(2));

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
        pin_variant(&abr, 3);

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
        let abr = fresh_abr(0);
        let ctx = HlsStreamContext::new(timeline.clone(), Arc::clone(&segments), Arc::clone(&abr));

        timeline.set_byte_position(200);
        timeline.set_segment_position(150);
        assert_eq!(ctx.segment_index(), Some(0));
        assert_eq!(ctx.variant_index(), Some(0));
    }
}
