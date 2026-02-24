#![forbid(unsafe_code)]

//! HLS stream context for lock-free segment/variant access.

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicUsize, Ordering},
};

use kithara_stream::{StreamContext, Timeline};

/// `StreamContext` for HLS segmented sources.
pub struct HlsStreamContext {
    timeline: Timeline,
    segment_index: Arc<AtomicU32>,
    variant_index: Arc<AtomicUsize>,
}

impl HlsStreamContext {
    pub fn new(
        timeline: Timeline,
        segment_index: Arc<AtomicU32>,
        variant_index: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            timeline,
            segment_index,
            variant_index,
        }
    }
}

impl StreamContext for HlsStreamContext {
    fn byte_offset(&self) -> u64 {
        self.timeline.byte_position()
    }

    fn segment_index(&self) -> Option<u32> {
        Some(self.segment_index.load(Ordering::Relaxed))
    }

    fn variant_index(&self) -> Option<usize> {
        Some(self.variant_index.load(Ordering::Relaxed))
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
        let segment = Arc::new(AtomicU32::new(5));
        let variant = Arc::new(AtomicUsize::new(2));

        let ctx =
            HlsStreamContext::new(timeline.clone(), Arc::clone(&segment), Arc::clone(&variant));

        assert_eq!(ctx.byte_offset(), 1000);
        assert_eq!(ctx.segment_index(), Some(5));
        assert_eq!(ctx.variant_index(), Some(2));

        // Atomics update
        timeline.set_byte_position(2000);
        segment.store(10, Ordering::Relaxed);
        variant.store(3, Ordering::Relaxed);

        assert_eq!(ctx.byte_offset(), 2000);
        assert_eq!(ctx.segment_index(), Some(10));
        assert_eq!(ctx.variant_index(), Some(3));
    }
}
