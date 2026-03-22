#![forbid(unsafe_code)]
// unimock macro generates code triggering ignored_unit_patterns
#![allow(clippy::ignored_unit_patterns)]

//! Source trait for sync random-access data.
//!
//! Sources provide sync random-access via `wait_range()` and `read_at()`.
//! Reader wraps this directly for `Read + Seek`.

use std::{error::Error as StdError, fmt, ops::Range};

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{
    Timeline, coordination::TransferCoordination, error::StreamResult, layout::LayoutIndex,
    media::MediaInfo, topology::Topology,
};

/// Phase of a source's wait/read lifecycle.
///
/// Each `Source` implementation returns the current phase from its
/// `phase()` method — a point-in-time snapshot for external observers
/// (audio pipeline, tracing, UI).
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

/// Non-retriable cross-variant boundary signal from [`ReadOutcome::VariantChange`].
#[derive(Debug)]
pub struct VariantChangeError;

impl fmt::Display for VariantChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("variant change: decoder recreation required")
    }
}

impl StdError for VariantChangeError {}

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
#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock(
        api = SourceMock,
        type Error = std::io::Error;
        type Topology = ();
        type Layout = ();
        type Coord = ();
        type Demand = ();
    )
)]
#[expect(clippy::len_without_is_empty)]
pub trait Source: Send + 'static {
    /// Error type.
    type Error: StdError + Send + Sync + 'static;
    /// Read-only media structure for this source.
    type Topology: Topology;
    /// Committed placement of logical items in the virtual byte space.
    type Layout: LayoutIndex;
    /// Shared runtime coordination between source and downloader.
    type Coord: TransferCoordination<Self::Demand>;
    /// On-demand request type used by the source-specific coordinator.
    type Demand: Clone + Send + Sync + 'static;

    /// Read-only media structure for this source.
    fn topology(&self) -> &Self::Topology;

    /// Committed placement handle for this source.
    fn layout(&self) -> &Self::Layout;

    /// Shared runtime coordination for this source.
    fn coord(&self) -> &Self::Coord;

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

    /// Point-in-time snapshot of the source phase for the given range.
    ///
    /// Returns the current [`SourcePhase`] without blocking. Used internally
    /// by `wait_range()` implementations for fast-path dispatch.
    fn phase_at(&self, range: Range<u64>) -> SourcePhase;

    /// Overall source readiness at the current timeline position.
    ///
    /// Uses the source's internal knowledge of chunk/segment boundaries
    /// to determine if the next read operation can proceed without blocking.
    ///
    /// Unlike `phase_at(range)` which checks a specific byte range,
    /// this method lets the source decide the appropriate granularity.
    ///
    /// Default checks a single byte at the current position.
    /// HLS overrides with segment-aware logic, File with 32KB-window logic.
    fn phase(&self) -> SourcePhase {
        let pos = self.timeline().byte_position();
        self.phase_at(pos..pos.saturating_add(1))
    }

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

    /// Signal that the given byte range will be needed soon.
    ///
    /// Non-blocking hint that allows the source to enqueue background
    /// fetch requests without entering the blocking [`wait_range`](Self::wait_range)
    /// path.  Called by the audio worker FSM when it discovers that a
    /// range is not ready and cannot block to wait for it.
    ///
    /// Segmented sources (HLS) use this to issue on-demand segment
    /// requests that wake the downloader.  Non-segmented sources
    /// (File) keep the default no-op because their downloader fetches
    /// sequentially and does not need explicit demand signals.
    fn demand_range(&self, _range: Range<u64>) {}

    /// Get shared playback timeline.
    ///
    /// Timeline is the single source of truth for playback state across all
    /// stream types (segmented and non-segmented).
    fn timeline(&self) -> Timeline {
        self.coord().timeline()
    }

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

    /// Commit the actual post-seek landing after `decoder.seek(...)`.
    ///
    /// Segmented sources can use this hook to reconcile source-local state
    /// with the authoritative landed reader position in [`Timeline`].
    ///
    /// Default no-op for sources that do not need post-seek reconciliation.
    fn commit_seek_landing(&mut self, _anchor: Option<SourceSeekAnchor>) {}
}

#[cfg(test)]
mod tests {
    use crate::DemandSlot;

    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[derive(Default)]
    struct TestCoord {
        demand: DemandSlot<()>,
        timeline: Timeline,
    }

    impl TransferCoordination<()> for TestCoord {
        fn timeline(&self) -> Timeline {
            self.timeline.clone()
        }

        fn demand(&self) -> &DemandSlot<()> {
            &self.demand
        }
    }

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
    fn phase_default_delegates_to_phase_at() {
        #[derive(Default)]
        struct ReadySource {
            coord: TestCoord,
        }
        impl Source for ReadySource {
            type Error = std::io::Error;
            type Topology = ();
            type Layout = ();
            type Coord = TestCoord;
            type Demand = ();
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
            fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
                SourcePhase::Ready
            }
            fn len(&self) -> Option<u64> {
                Some(100)
            }

            fn topology(&self) -> &Self::Topology {
                &()
            }

            fn layout(&self) -> &Self::Layout {
                &()
            }

            fn coord(&self) -> &Self::Coord {
                &self.coord
            }
        }
        let source = ReadySource::default();
        // Default phase() delegates to phase_at(0..1) since timeline position is 0.
        assert_eq!(source.phase(), SourcePhase::Ready);
    }
}
