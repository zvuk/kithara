#![forbid(unsafe_code)]
// unimock macro generates code triggering ignored_unit_patterns
#![allow(clippy::ignored_unit_patterns)]

//! Source trait for sync random-access data.
//!
//! Sources provide sync random-access via `wait_range()` and `read_at()`.
//! Reader wraps this directly for `Read + Seek`.

use std::{error::Error as StdError, ops::Range};

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{Timeline, error::StreamResult, media::MediaInfo};

/// Phase of a source's wait/read lifecycle.
///
/// Shared FSM core for both `FileSource` and `HlsSource`.
/// Sources build a [`SourcePhaseView`] from their local state,
/// then call [`SourcePhase::classify`] to derive the current phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SourcePhase {
    /// Cancelled — terminal, source will not produce more data.
    Cancelled,
    /// End of stream reached.
    Eof,
    /// Requested range is available for non-blocking read.
    Ready,
    /// Active seek in progress — decoder should be interrupted.
    Seeking,
    /// Stopped before EOF — terminal unless range is ready (drain).
    Stopped,
    /// Default: data not yet available, no specific sub-state.
    #[default]
    Waiting,
    /// On-demand request already in flight for this seek epoch.
    WaitingDemand,
    /// Metadata lookup needed before data can be requested.
    WaitingMetadata,
}

/// Input view for [`SourcePhase::classify`].
///
/// Sources populate this from their local observations (cancel tokens,
/// timeline state, segment readiness, etc.) without holding `&mut self`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourcePhaseView {
    /// Cancellation token is set.
    pub cancelled: bool,
    /// Read position is past the effective end of stream.
    pub past_eof: bool,
    /// Requested byte range is available for non-blocking read.
    pub range_ready: bool,
    /// Timeline is flushing (active seek).
    pub seeking: bool,
    /// Source has been stopped.
    pub stopped: bool,
    /// On-demand request already pending for this seek epoch.
    pub waiting_demand: bool,
    /// Metadata lookup miss — segment cannot be resolved yet.
    pub waiting_metadata: bool,
}

impl SourcePhase {
    /// Classify source phase from an observation view.
    ///
    /// Priority order (highest wins):
    /// 1. `Cancelled` — overrides everything
    /// 2. `Stopped` (without ready data) — `Eof` if past end, else `Stopped`
    /// 3. `Ready` — data available, preempts seeking/eof
    /// 4. `Seeking` — decoder interrupt
    /// 5. `Eof` — end of stream
    /// 6. `WaitingMetadata` — metadata miss
    /// 7. `WaitingDemand` — on-demand in flight
    /// 8. `Waiting` — default
    #[must_use]
    pub fn classify(view: SourcePhaseView) -> Self {
        if view.cancelled {
            return Self::Cancelled;
        }
        if view.stopped && !view.range_ready {
            return if view.past_eof {
                Self::Eof
            } else {
                Self::Stopped
            };
        }
        if view.range_ready {
            return Self::Ready;
        }
        if view.seeking {
            return Self::Seeking;
        }
        if view.past_eof {
            return Self::Eof;
        }
        if view.waiting_metadata {
            return Self::WaitingMetadata;
        }
        if view.waiting_demand {
            return Self::WaitingDemand;
        }
        Self::Waiting
    }
}

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
    type Error: StdError + Send + Sync + 'static;

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

    /// Check whether all bytes in `range` are available for non-blocking read.
    ///
    /// Used by the shared audio worker to decide whether `fetch_next()` can
    /// proceed without blocking. Conservative: returning `false` when data
    /// is actually available is safe (the track is simply skipped this
    /// iteration), but returning `true` when data is missing would block
    /// the shared worker thread.
    ///
    /// Default returns `false` (unknown sources are assumed not ready).
    fn is_range_ready(&self, range: Range<u64>) -> bool {
        let _ = range;
        false
    }

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

    #[kithara::test]
    fn source_phase_defaults_to_waiting() {
        assert_eq!(SourcePhase::default(), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn source_phase_classify_prefers_cancelled() {
        // Cancelled overrides everything, even if range is ready.
        let view = SourcePhaseView {
            cancelled: true,
            range_ready: true,
            seeking: true,
            past_eof: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Cancelled);
    }

    #[kithara::test]
    fn source_phase_classify_prefers_ready_over_seeking() {
        let view = SourcePhaseView {
            range_ready: true,
            seeking: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Ready);
    }

    #[kithara::test]
    fn source_phase_classify_returns_waiting_demand() {
        let view = SourcePhaseView {
            waiting_demand: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::WaitingDemand);
    }

    #[kithara::test]
    fn source_phase_classify_returns_waiting_metadata() {
        let view = SourcePhaseView {
            waiting_metadata: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::WaitingMetadata);
    }

    #[kithara::test]
    fn source_phase_classify_returns_stopped_before_eof() {
        // Stopped + not range_ready + not past_eof = Stopped (not Eof)
        let view = SourcePhaseView {
            stopped: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Stopped);
    }

    #[kithara::test]
    fn source_phase_classify_stopped_past_eof_returns_eof() {
        // Stopped + past_eof = Eof (graceful drain)
        let view = SourcePhaseView {
            stopped: true,
            past_eof: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Eof);
    }

    #[kithara::test]
    fn source_phase_classify_seeking_without_ready() {
        let view = SourcePhaseView {
            seeking: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Seeking);
    }

    #[kithara::test]
    fn source_phase_classify_eof_without_stopped() {
        let view = SourcePhaseView {
            past_eof: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Eof);
    }

    // --- Priority edge tests (ordering contract) ---

    #[kithara::test]
    fn source_phase_classify_stopped_beats_seeking() {
        let view = SourcePhaseView {
            stopped: true,
            seeking: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Stopped);
    }

    #[kithara::test]
    fn source_phase_classify_seeking_beats_waiting_demand() {
        let view = SourcePhaseView {
            seeking: true,
            waiting_demand: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Seeking);
    }

    #[kithara::test]
    fn source_phase_classify_eof_beats_waiting_metadata() {
        let view = SourcePhaseView {
            past_eof: true,
            waiting_metadata: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Eof);
    }

    #[kithara::test]
    fn source_phase_classify_waiting_metadata_beats_waiting_demand() {
        let view = SourcePhaseView {
            waiting_metadata: true,
            waiting_demand: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::WaitingMetadata);
    }

    #[kithara::test]
    fn source_phase_classify_empty_view_returns_waiting() {
        let view = SourcePhaseView::default();
        assert_eq!(SourcePhase::classify(view), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn source_phase_classify_stopped_with_range_ready_returns_ready() {
        // Stopped but data is available — allow drain.
        let view = SourcePhaseView {
            stopped: true,
            range_ready: true,
            ..Default::default()
        };
        assert_eq!(SourcePhase::classify(view), SourcePhase::Ready);
    }

    #[kithara::test]
    fn is_range_ready_default_returns_false() {
        use kithara_storage::WaitOutcome;

        struct StubSource;
        impl Source for StubSource {
            type Error = std::io::Error;
            fn wait_range(
                &mut self,
                _range: Range<u64>,
                _timeout: Duration,
            ) -> StreamResult<WaitOutcome, Self::Error> {
                Ok(WaitOutcome::Ready)
            }
            fn read_at(
                &mut self,
                _offset: u64,
                _buf: &mut [u8],
            ) -> StreamResult<ReadOutcome, Self::Error> {
                Ok(ReadOutcome::Data(0))
            }
            fn len(&self) -> Option<u64> {
                Some(100)
            }
            fn timeline(&self) -> Timeline {
                Timeline::new()
            }
        }
        let source = StubSource;
        assert!(!source.is_range_ready(0..10));
    }
}
