#![forbid(unsafe_code)]
// unimock macro generates code triggering ignored_unit_patterns
#![allow(clippy::ignored_unit_patterns)]

//! Source trait for sync random-access data.
//!
//! Sources provide sync random-access via `wait_range()` and `read_at()`.
//! Reader wraps this directly for `Read + Seek`.

use std::ops::Range;

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{Timeline, error::StreamResult, media::MediaInfo};

/// Outcome of a `Source::read_at` call.
///
/// Distinguishes between normal data reads (including true EOF) and
/// variant/format changes that require decoder recreation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Read `n` bytes. `n == 0` means true end-of-stream.
    Data(usize),
    /// Variant/format change at this offset. Caller must recreate the
    /// decoder and call `clear_variant_fence()` before reads succeed.
    /// Zero bytes were read — fence fires BEFORE any data is touched.
    VariantChange,
    /// Resource was evicted between `wait_range` (metadata ready) and
    /// `read_at` (actual I/O). Caller should retry from `wait_range`.
    Retry,
}

/// Time-first seek anchor resolved by a segmented source.
///
/// Represents a deterministic mapping from target playback time to a byte
/// position and segment context inside the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSeekAnchor {
    pub byte_offset: u64,
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub segment_index: Option<u32>,
    pub variant_index: Option<usize>,
}

/// Sync random-access source.
///
/// Provides sync interface for waiting and reading data at arbitrary offsets.
/// Reader wraps this directly to provide `Read + Seek`.
///
/// Methods take `&mut self` to allow sources to maintain internal state
/// (e.g., progress tracking, segment index updates).
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = SourceMock, type Error = std::io::Error;))]
#[expect(clippy::len_without_is_empty)]
pub trait Source: Send + 'static {
    /// Error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Wait for data in range to be available.
    ///
    /// `timeout` is the maximum wait time before returning an implementation-defined
    /// non-ready outcome (typically [`WaitOutcome::Interrupted`]).
    ///
    /// # Errors
    ///
    /// Returns an error if the wait is cancelled or the underlying storage fails.
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error>;

    /// Read data at offset into buffer.
    ///
    /// Returns [`ReadOutcome::Data(n)`] with the number of bytes read (0 = true EOF),
    /// or [`ReadOutcome::VariantChange`] when a variant fence blocks the read.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails or the source is in an invalid state.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error>;

    /// Total length if known.
    fn len(&self) -> Option<u64>;

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

    /// Clear variant fence, allowing reads from the next variant.
    ///
    /// Called when the decoder is recreated after ABR switch.
    /// Default no-op for non-HLS sources.
    fn clear_variant_fence(&mut self) {}

    /// Wake any blocked `wait_range()` calls.
    ///
    /// Called after `Timeline::initiate_seek()` to ensure immediate response
    /// from threads sleeping on condvars. Default no-op for sources without
    /// blocking waits.
    fn notify_waiting(&self) {}

    /// Create a callback that wakes blocked `wait_range()` without holding
    /// the `SharedStream` mutex.
    ///
    /// The returned closure captures only the underlying condvar/notify
    /// primitive, so calling it from the main thread cannot deadlock even
    /// when the worker thread holds the `SharedStream` lock inside `read()`.
    ///
    /// Default returns `None` (no blocking waits to wake).
    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        None
    }

    /// Set current seek epoch for stale request invalidation.
    ///
    /// HLS uses this to drop in-flight network/segment requests that belong
    /// to previous seeks. Non-seek-aware sources keep the default no-op.
    fn set_seek_epoch(&mut self, _seek_epoch: u64) {}

    /// Get shared playback timeline.
    ///
    /// Timeline is the single source of truth for playback state across all
    /// stream types (segmented and non-segmented).
    fn timeline(&self) -> Timeline;

    /// Resolve `position` to a source-specific seek anchor.
    ///
    /// Segmented sources (HLS) should map time to a deterministic segment
    /// boundary and byte offset. Non-segmented sources return `Ok(None)`.
    ///
    /// The caller is expected to set stream position to `byte_offset` and
    /// perform decoder reset/recreation using this anchor.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot resolve the anchor.
    fn seek_time_anchor(
        &mut self,
        _position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>, Self::Error> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    // Dummy test to verify trait compiles
    #[kithara::test]
    fn test_source_trait_object_safety() {
        // Source is not object-safe due to associated types,
        // but we can verify it compiles with concrete types
        fn _accepts_source<S: Source>(_s: S) {}
    }
}
