#![forbid(unsafe_code)]

//! Source trait for async random-access data.
//!
//! Sources provide async random-access via `wait_range()` and `read_at()`.
//! Backend bridges this to sync `Read + Seek` via channels.

use std::ops::Range;

use async_trait::async_trait;
use kithara_storage::WaitOutcome;

use crate::{error::StreamResult, media::MediaInfo};

/// Async random-access source.
///
/// Provides async interface for waiting and reading data at arbitrary offsets.
/// Backend wraps this to provide sync `Read + Seek` via channels.
///
/// Methods take `&mut self` to allow sources to fetch data on demand
/// (e.g., HLS segments, progressive download chunks).
#[async_trait]
pub trait Source: Send + 'static {
    /// Item type (bytes for raw streams, samples for decoded audio).
    type Item: Send;

    /// Error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Wait for data in range to be available.
    ///
    /// May fetch data if not yet available.
    /// Returns `WaitOutcome::Ready` when range is available,
    /// `WaitOutcome::Eof` if EOF reached before range end.
    async fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error>;

    /// Read data at offset into buffer.
    ///
    /// Returns number of bytes read. May return less than `buf.len()`.
    async fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error>;

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
    /// For segmented sources (HLS), returns the range of the current segment.
    /// For non-segmented sources (File), returns `0..len` or `0..u64::MAX` if unknown.
    /// Used by decoder to detect segment boundaries for format change handling.
    fn current_segment_range(&self) -> Range<u64> {
        0..self.len().unwrap_or(u64::MAX)
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
