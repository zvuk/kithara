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
    use super::*;

    #[test]
    fn test_null_stream_context_defaults() {
        let pos = Arc::new(AtomicU64::new(0));
        let ctx = NullStreamContext::new(Arc::clone(&pos));
        assert_eq!(ctx.byte_offset(), 0);
        assert_eq!(ctx.segment_index(), None);
        assert_eq!(ctx.variant_index(), None);
    }

    #[test]
    fn test_null_stream_context_tracks_byte_offset() {
        let pos = Arc::new(AtomicU64::new(0));
        let ctx = NullStreamContext::new(Arc::clone(&pos));
        pos.store(12345, Ordering::Relaxed);
        assert_eq!(ctx.byte_offset(), 12345);
    }
}
