//! Read-only stream context for the decoder.
//!
//! [`StreamContext`] provides atomic access to byte position and
//! segment/variant information without locking.

#![forbid(unsafe_code)]

use crate::Timeline;

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
    timeline: Timeline,
}

impl NullStreamContext {
    #[must_use]
    pub fn new(timeline: Timeline) -> Self {
        Self { timeline }
    }
}

impl StreamContext for NullStreamContext {
    fn byte_offset(&self) -> u64 {
        self.timeline.byte_position()
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
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test(wasm)]
    #[case::initial(0)]
    #[case::advanced(12_345)]
    fn test_null_stream_context_reads_byte_offset(#[case] offset: u64) {
        let timeline = Timeline::new();
        let ctx = NullStreamContext::new(timeline.clone());
        timeline.set_byte_position(offset);
        assert_eq!(ctx.byte_offset(), offset);
        assert_eq!(ctx.segment_index(), None);
        assert_eq!(ctx.variant_index(), None);
    }

    #[kithara::test]
    fn stream_context_mock_api_is_generated() {
        let _ = StreamContextMock::byte_offset;
    }
}
