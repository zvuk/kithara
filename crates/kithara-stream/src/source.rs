#![forbid(unsafe_code)]

//! Source trait for sync random-access data.
//!
//! Sources provide sync random-access via `wait_range()` and `read_at()`.
//! Reader wraps this directly for `Read + Seek`.

use std::ops::Range;

use kithara_storage::WaitOutcome;

use crate::{error::StreamResult, media::MediaInfo};

/// Sync random-access source.
///
/// Provides sync interface for waiting and reading data at arbitrary offsets.
/// Reader wraps this directly to provide `Read + Seek`.
///
/// Methods take `&mut self` to allow sources to maintain internal state
/// (e.g., progress tracking, segment index updates).
pub trait Source: Send + 'static {
    /// Item type (bytes for raw streams, samples for decoded audio).
    type Item: Send;

    /// Error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Wait for data in range to be available.
    ///
    /// Blocks until data is available or EOF is reached.
    /// Returns `WaitOutcome::Ready` when range is available,
    /// `WaitOutcome::Eof` if EOF reached before range end.
    fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error>;

    /// Read data at offset into buffer.
    ///
    /// Returns number of bytes read. May return less than `buf.len()`.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error>;

    /// Total length if known.
    fn len(&self) -> Option<u64>;

    /// Check if source is empty.
    fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Get media info if available.
    fn media_info(&self) -> Option<MediaInfo> {
        None
    }

    /// Get current segment byte range.
    ///
    /// For segmented sources (HLS), returns `Some(range)` of the current segment.
    /// For non-segmented sources (File), returns `None`.
    /// Used by decoder to detect segment boundaries for format change handling.
    fn current_segment_range(&self) -> Option<Range<u64>> {
        None
    }

    /// Get byte range of the first segment with current format after a format change.
    ///
    /// For HLS ABR switch: returns the first segment of the new variant which contains
    /// init data (ftyp/moov). This is where the decoder should be recreated.
    ///
    /// Returns `None` if no format change occurred or for non-segmented sources.
    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Dummy test to verify trait compiles
    #[test]
    fn test_source_trait_object_safety() {
        // Source is not object-safe due to associated types,
        // but we can verify it compiles with concrete types
        fn _accepts_source<S: Source>(_s: S) {}
    }
}
