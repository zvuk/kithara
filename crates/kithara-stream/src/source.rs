#![forbid(unsafe_code)]
// unimock macro generates code triggering ignored_unit_patterns
#![allow(clippy::ignored_unit_patterns)]

//! Source trait for sync random-access data.
//!
//! Sources provide sync random-access via `wait_range()` and `read_at()`.
//! Reader wraps this directly for `Read + Seek`.

use std::{error::Error as StdError, fmt, num::NonZeroUsize, ops::Range, sync::Arc};

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{Timeline, error::StreamResult, media::MediaInfo};

/// Per-segment metadata exposed by segmented sources (HLS).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SegmentDescriptor {
    /// Byte range in the source's virtual stream.
    pub byte_range: Range<u64>,
    /// Absolute decode time at the start of this segment (cumulative
    /// EXTINF over preceding segments).
    pub decode_time: Duration,
    /// Segment duration (EXTINF).
    pub duration: Duration,
    /// Segment index within the variant.
    pub segment_index: u32,
    /// Variant the descriptor was resolved against.
    pub variant_index: usize,
}

impl SegmentDescriptor {
    #[must_use]
    pub fn new(
        byte_range: Range<u64>,
        decode_time: Duration,
        duration: Duration,
        segment_index: u32,
        variant_index: usize,
    ) -> Self {
        Self {
            byte_range,
            decode_time,
            duration,
            segment_index,
            variant_index,
        }
    }
}

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
    /// Default: data not yet available, no specific sub-state.
    #[default]
    Waiting,
    /// On-demand request already in flight for this seek epoch.
    WaitingDemand,
    /// Metadata lookup needed before data can be requested.
    WaitingMetadata,
}

/// Reason a [`ReadOutcome::Pending`] was returned — i.e. why the source
/// did not make progress this call. Each variant maps to a distinct
/// caller action; there is no overlap and no string-matching required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PendingReason {
    /// A seek is pending (consumer flagged the timeline). The caller
    /// must abort the current read and let the seek apply — do **not**
    /// retry from the same byte offset.
    SeekPending,
    /// Data is not yet available at the requested range. Transient —
    /// caller may retry after backoff.
    NotReady,
    /// Source crossed a variant boundary at this offset. Caller must
    /// recreate the decoder and call
    /// [`Source::clear_variant_fence`] before reads succeed. Zero bytes
    /// were touched — the fence fires BEFORE any data is read.
    VariantChange,
    /// Resource was evicted between [`Source::wait_range`] (metadata
    /// ready) and [`Source::read_at`] (actual I/O). Caller should
    /// retry from `wait_range`, not from the same byte offset.
    Retry,
}

impl fmt::Display for PendingReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SeekPending => "seek pending",
            Self::NotReady => "data not ready",
            Self::VariantChange => "variant change: decoder recreation required",
            Self::Retry => "resource evicted, retry wait_range",
        })
    }
}

impl StdError for PendingReason {}

/// Outcome of a [`Source::read_at`] call.
///
/// Each variant has distinct caller semantics — there is no
/// overload of a numeric zero. `Bytes` carries a typed
/// [`NonZeroUsize`] so the type system guarantees forward progress;
/// `Pending` carries an explicit [`PendingReason`]; `Eof` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Source produced `count` bytes (`count > 0` by construction).
    Bytes(NonZeroUsize),
    /// Source did not make progress this call. See [`PendingReason`]
    /// for the precise cause and required caller action.
    Pending(PendingReason),
    /// Natural end of stream — no more bytes will ever come from this
    /// source at this offset.
    Eof,
}

/// Time-first seek anchor resolved by a segmented source.
///
/// Represents a deterministic mapping from target playback time to a byte
/// position and segment context inside the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, derive_setters::Setters)]
#[setters(prefix = "with_", strip_option)]
#[non_exhaustive]
pub struct SourceSeekAnchor {
    pub byte_offset: u64,
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub segment_index: Option<u32>,
    pub variant_index: Option<usize>,
}

impl SourceSeekAnchor {
    /// Create a minimal anchor with just a byte offset and segment start
    /// time. Optional fields default to `None`; set them via the
    /// `with_*` builders.
    #[must_use]
    pub fn new(byte_offset: u64, segment_start: Duration) -> Self {
        Self {
            byte_offset,
            segment_start,
            ..Self::default()
        }
    }
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
    unimock(api = SourceMock)
)]
#[expect(clippy::len_without_is_empty)]
pub trait Source: Send + Sync + 'static {
    /// Get shared playback timeline.
    ///
    /// Timeline is the single source of truth for playback state across all
    /// stream types (segmented and non-segmented). Sources own their
    /// Timeline and hand out cheap Arc clones to downstream consumers
    /// (reader, audio FSM, Downloader peers).
    fn timeline(&self) -> Timeline;

    /// Wait for data in range to be available.
    ///
    /// `timeout` is the maximum wait time before returning an
    /// implementation-defined non-ready outcome (typically a typed
    /// "budget exceeded" error). Pass `None` to wait until the range
    /// is ready or the source's internal cancel signal fires — used
    /// for [`Stream::seek`](crate::Stream::seek), where giving up on
    /// a timer would silently drop the seek under slow connections.
    /// `Some(WAIT_RANGE_TIMEOUT)` is the cooperative-yield path used
    /// by the audio worker's read loop.
    ///
    /// # Errors
    ///
    /// Returns an error if the wait is cancelled or the underlying storage fails.
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome>;

    /// Read data at offset into buffer.
    ///
    /// Returns [`ReadOutcome::Bytes`] with a non-zero byte count on
    /// progress, [`ReadOutcome::Pending`] with a typed
    /// [`PendingReason`] when no progress is possible this call (seek
    /// pending, variant fence, eviction), or [`ReadOutcome::Eof`] at
    /// natural end-of-stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails or the source is in an invalid state.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome>;

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
    ///
    /// Streaming sources may block briefly until the HTTP response headers
    /// arrive (Content-Length discovery).
    fn len(&self) -> Option<u64>;

    /// Get media info if available.
    fn media_info(&self) -> Option<MediaInfo> {
        None
    }

    /// Current ABR handle for runtime mode/bandwidth control.
    ///
    /// Adaptive sources (HLS) return the peer's `AbrHandle` so callers —
    /// queue, FFI, UI — can switch variant or cap bandwidth mid-playback.
    /// Non-adaptive sources (File) keep the default `None`.
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
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

    /// Switch layout to the ABR target variant.
    ///
    /// Must be called right before decoder recreation so the new decoder
    /// reads from the correct variant's byte map. Separate from
    /// `format_change_segment_range()` to avoid switching layout while
    /// the old decoder is still reading.
    fn commit_variant_layout(&mut self) {}

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
    fn seek_time_anchor(&mut self, _position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        Ok(None)
    }

    /// Commit the actual post-seek landing after `decoder.seek(...)`.
    ///
    /// Segmented sources can use this hook to reconcile source-local state
    /// with the authoritative landed reader position in [`Timeline`].
    ///
    /// Default no-op for sources that do not need post-seek reconciliation.
    fn commit_seek_landing(&mut self, _anchor: Option<SourceSeekAnchor>) {}

    /// Build a fresh reader-side hooks instance.
    ///
    /// Returned by Source-impls that want to expose reader-side events
    /// (`HlsSource`, `FileSource`). The audio pipeline takes the hook
    /// at decoder creation/recreation time and threads it into the
    /// `HookedDecoder` wrapper. Default `None` keeps mock and test
    /// sources unhooked.
    ///
    /// `take_*` is a misnomer: each call must return a **fresh** hook
    /// instance, because decoder recreation (ABR / format change)
    /// rebuilds the wrapper and the new hook needs a clean state
    /// cursor.
    fn take_reader_hooks(&mut self) -> Option<crate::SharedHooks> {
        None
    }

    /// Optional shared segment-layout handle for segment-aware decoders.
    ///
    /// Segment-aware decoders (fMP4 segment demuxer) call this once at
    /// open to grab a lock-free, Arc-shareable view over the segment
    /// table — independent of the byte cursor passed to the decoder
    /// through `Read + Seek`. Default `None` for non-segmented sources.
    fn as_segment_layout(&self) -> Option<Arc<dyn SegmentLayout>> {
        None
    }
}

/// Segment-table view exposed by segmented sources (HLS, fragmented
/// file-mp4).
///
/// Carries the segment metadata that segment-aware decoders need to
/// route reads — `init_segment_range` (ftyp+moov / `EXT-X-MAP`),
/// `segment_at_time`, `segment_after_byte`, `segment_count`, and total
/// `len`. Has no I/O surface: the byte cursor is the decoder's
/// `Read + Seek` handle, queried independently. Sources that aren't
/// segment-aware return `None` from [`Source::as_segment_layout`].
#[expect(
    clippy::len_without_is_empty,
    reason = "len() returns Option<u64> for total bytes — emptiness has no meaningful definition for a segmented source"
)]
pub trait SegmentLayout: Send + Sync + 'static {
    /// Init segment range (e.g. ftyp+moov from `EXT-X-MAP`) for the
    /// current layout variant. Returns `None` until the init segment is
    /// announced.
    fn init_segment_range(&self) -> Option<Range<u64>>;

    /// Locate the segment whose `[decode_time, decode_time + duration)`
    /// covers `t`. Resolves against the source's *current layout
    /// variant* — same variant `init_segment_range` describes.
    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor>;

    /// Next segment whose byte range starts at or after `byte_offset`.
    /// Used for sequential play after the current segment is consumed.
    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor>;

    /// Total number of segments in the current layout variant.
    fn segment_count(&self) -> Option<u32>;

    /// Total byte length across all segments. Used to compute total
    /// duration when the source can't provide a direct value.
    fn len(&self) -> Option<u64>;
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

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
            timeline: Timeline,
        }
        impl Source for ReadySource {
            fn timeline(&self) -> Timeline {
                self.timeline.clone()
            }
            fn wait_range(
                &mut self,
                _range: Range<u64>,
                _timeout: Option<Duration>,
            ) -> StreamResult<WaitOutcome> {
                Ok(WaitOutcome::Ready)
            }
            fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
                Ok(ReadOutcome::Eof)
            }
            fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
                SourcePhase::Ready
            }
            fn len(&self) -> Option<u64> {
                Some(100)
            }
        }
        let source = ReadySource::default();
        // Default phase() delegates to phase_at(0..1) since timeline position is 0.
        assert_eq!(source.phase(), SourcePhase::Ready);
    }
}
