//! Read-only stream context for the decoder.
//!
//! [`StreamContext`] provides atomic access to byte position and
//! segment/variant information without locking.

#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Read-only view of stream state for the decoder.
///
/// Provides byte position and segment context. Implementations expose
/// atomic state from Reader and Source without locking.
#[cfg_attr(test, unimock::unimock(api = StreamContextMock))]
pub trait StreamContext: Send + Sync {
    /// Current byte offset in the underlying data stream.
    fn byte_offset(&self) -> u64;
    /// Current segment index (`None` for non-segmented sources).
    fn segment_index(&self) -> Option<u32>;
    /// Current variant index (`None` for non-segmented sources).
    fn variant_index(&self) -> Option<usize>;
}

/// `StreamContext` for non-segmented sources (progressive files).
///
/// Always returns `None` for segment/variant.
pub struct NullStreamContext {
    byte_offset: Arc<AtomicU64>,
}

impl NullStreamContext {
    pub fn new(byte_offset: Arc<AtomicU64>) -> Self {
        Self { byte_offset }
    }
}

impl StreamContext for NullStreamContext {
    fn byte_offset(&self) -> u64 {
        self.byte_offset.load(Ordering::Relaxed)
    }

    fn segment_index(&self) -> Option<u32> {
        None
    }

    fn variant_index(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::initial(0)]
    #[case::advanced(12_345)]
    fn test_null_stream_context_reads_byte_offset(#[case] offset: u64) {
        let pos = Arc::new(AtomicU64::new(0));
        let ctx = NullStreamContext::new(Arc::clone(&pos));
        pos.store(offset, Ordering::Relaxed);
        assert_eq!(ctx.byte_offset(), offset);
        assert_eq!(ctx.segment_index(), None);
        assert_eq!(ctx.variant_index(), None);
    }

    #[test]
    fn stream_context_mock_api_is_generated() {
        let _ = StreamContextMock::byte_offset;
    }
}
