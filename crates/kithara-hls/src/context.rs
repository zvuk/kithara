#![forbid(unsafe_code)]

//! HLS stream context for lock-free segment/variant access.

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use kithara_stream::StreamContext;

/// `StreamContext` for HLS segmented sources.
pub struct HlsStreamContext {
    byte_offset: Arc<AtomicU64>,
    segment_index: Arc<AtomicU32>,
    variant_index: Arc<AtomicUsize>,
}

impl HlsStreamContext {
    pub fn new(
        byte_offset: Arc<AtomicU64>,
        segment_index: Arc<AtomicU32>,
        variant_index: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            byte_offset,
            segment_index,
            variant_index,
        }
    }
}

impl StreamContext for HlsStreamContext {
    fn byte_offset(&self) -> u64 {
        self.byte_offset.load(Ordering::Relaxed)
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
    use super::*;

    #[test]
    fn test_hls_stream_context_reads_atomics() {
        let byte_offset = Arc::new(AtomicU64::new(1000));
        let segment = Arc::new(AtomicU32::new(5));
        let variant = Arc::new(AtomicUsize::new(2));

        let ctx = HlsStreamContext::new(
            Arc::clone(&byte_offset),
            Arc::clone(&segment),
            Arc::clone(&variant),
        );

        assert_eq!(ctx.byte_offset(), 1000);
        assert_eq!(ctx.segment_index(), Some(5));
        assert_eq!(ctx.variant_index(), Some(2));

        // Atomics update
        byte_offset.store(2000, Ordering::Relaxed);
        segment.store(10, Ordering::Relaxed);
        variant.store(3, Ordering::Relaxed);

        assert_eq!(ctx.byte_offset(), 2000);
        assert_eq!(ctx.segment_index(), Some(10));
        assert_eq!(ctx.variant_index(), Some(3));
    }
}
