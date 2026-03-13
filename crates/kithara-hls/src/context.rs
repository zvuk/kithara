#![forbid(unsafe_code)]

//! HLS stream context for lock-free segment/variant access.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara_platform::Mutex;
use kithara_stream::{StreamContext, Timeline};

use crate::download_state::DownloadState;

/// `StreamContext` for HLS segmented sources.
pub struct HlsStreamContext {
    segments: Arc<Mutex<DownloadState>>,
    timeline: Timeline,
    variant_index: Arc<AtomicUsize>,
}

impl HlsStreamContext {
    pub fn new(
        timeline: Timeline,
        segments: Arc<Mutex<DownloadState>>,
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

    fn segment_index(&self) -> Option<u32> {
        let offset = self.timeline.byte_position();
        let segments = self.segments.lock_sync();
        #[expect(clippy::cast_possible_truncation, reason = "segment index fits in u32")]
        segments
            .find_at_offset(offset)
            .or_else(|| segments.last())
            .map(|segment| segment.segment_index as u32)
    }

    fn variant_index(&self) -> Option<usize> {
        let offset = self.timeline.byte_position();
        let segments = self.segments.lock_sync();
        segments
            .find_at_offset(offset)
            .or_else(|| segments.last())
            .map(|segment| segment.variant)
            .or_else(|| Some(self.variant_index.load(Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn test_hls_stream_context_reads_atomics() {
        let timeline = Timeline::new();
        timeline.set_byte_position(1000);
        let segments = Arc::new(Mutex::new(DownloadState::new()));
        segments
            .lock_sync()
            .push(crate::download_state::LoadedSegment {
                variant: 2,
                segment_index: 5,
                byte_offset: 1000,
                init_len: 0,
                media_len: 200,
                init_url: None,
                media_url: url::Url::parse("https://example.com/seg.m4s").unwrap(),
            });
        let variant = Arc::new(AtomicUsize::new(2));

        let ctx = HlsStreamContext::new(
            timeline.clone(),
            Arc::clone(&segments),
            Arc::clone(&variant),
        );

        assert_eq!(ctx.byte_offset(), 1000);
        assert_eq!(ctx.segment_index(), Some(5));
        assert_eq!(ctx.variant_index(), Some(2));

        // Atomics update
        timeline.set_byte_position(2000);
        segments
            .lock_sync()
            .push(crate::download_state::LoadedSegment {
                variant: 3,
                segment_index: 10,
                byte_offset: 2000,
                init_len: 0,
                media_len: 200,
                init_url: None,
                media_url: url::Url::parse("https://example.com/seg-2.m4s").unwrap(),
            });
        variant.store(3, Ordering::Relaxed);

        assert_eq!(ctx.byte_offset(), 2000);
        assert_eq!(ctx.segment_index(), Some(10));
        assert_eq!(ctx.variant_index(), Some(3));
    }
}
