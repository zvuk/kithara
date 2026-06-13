#![forbid(unsafe_code)]

use std::{error::Error as StdError, fmt, num::NonZeroUsize, ops::Range, sync::Arc};

use kithara_platform::{
    maybe_send::{MaybeSend, MaybeSync},
    time::Duration,
};
use kithara_storage::WaitOutcome;
use kithara_test_utils::kithara;

use crate::{
    error::StreamResult,
    media::MediaInfo,
    playhead::{PlayheadRead, PlayheadWrite},
    seek_state::{Activity, SeekControl, SeekObserve},
    wake::DeferredWake,
};

/// Per-segment metadata exposed by segmented sources (HLS).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SegmentDescriptor {
    /// Absolute decode time at the start of this segment (cumulative
    /// EXTINF over preceding segments).
    pub decode_time: Duration,
    /// Segment duration (EXTINF).
    pub duration: Duration,
    /// Byte range in the source's virtual stream.
    pub byte_range: Range<u64>,
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
            decode_time,
            duration,
            byte_range,
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
    /// caller may retry after backoff. The inner [`NotReadyCause`] tells
    /// which point in the read pipeline failed to make progress (wait
    /// budget exhausted, wait interrupted, source-side pending).
    NotReady(NotReadyCause),
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

/// Concrete cause for a [`PendingReason::NotReady`].
///
/// Carried as the typed payload of `NotReady` so the `io::Error` that
/// `impl Read for Stream` produces names the real stall site without
/// requiring decoder-side instrumentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotReadyCause {
    /// `wait_range` returned `WaitBudgetExceeded` for `MAX_WAIT_SPINS`
    /// iterations — the source kept signalling "not yet" past the read
    /// budget. Typical when a fetch is slower than the read deadline.
    WaitBudgetExhausted,
    /// `wait_range` returned `Interrupted` without an active flush, also
    /// past the spin budget — the downloader woke us but range still
    /// wasn't satisfied. Typical sign of a flapping ABR/eviction race.
    WaitInterrupted,
    /// `wait_range` reported ready but `read_at` then returned `Pending`
    /// with a non-`Retry` reason — surfaced verbatim from the source.
    SourcePending,
}

impl fmt::Display for NotReadyCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WaitBudgetExhausted => "wait budget exhausted",
            Self::WaitInterrupted => "wait interrupted, no flush",
            Self::SourcePending => "source returned pending after wait ready",
        })
    }
}

impl fmt::Display for PendingReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SeekPending => f.write_str("seek pending"),
            Self::NotReady(cause) => write!(f, "data not ready ({cause})"),
            Self::VariantChange => f.write_str("variant change: decoder recreation required"),
            Self::Retry => f.write_str("resource evicted, retry wait_range"),
        }
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct SourceSeekAnchor {
    #[builder(default)]
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub segment_index: Option<u32>,
    pub variant_index: Option<usize>,
    #[builder(default)]
    pub byte_offset: u64,
}

/// Sync random-access source.
///
/// Provides sync interface for waiting and reading data at arbitrary offsets.
/// Reader wraps this directly to provide `Read + Seek`.
///
/// Methods take `&mut self` to allow sources to maintain internal state
/// (e.g., progress tracking, segment index updates).
#[kithara::mock(api = SourceMock)]
pub trait Source: MaybeSend + MaybeSync + 'static {
    /// Current ABR handle for runtime mode/bandwidth control.
    ///
    /// Adaptive sources (HLS) return the peer's `AbrHandle` so callers —
    /// queue, FFI, UI — can switch variant or cap bandwidth mid-playback.
    /// Non-adaptive sources (File) keep the default `None`.
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        None
    }

    /// Advance the byte cursor by `n` bytes after a successful read.
    fn advance(&self, n: u64);

    /// Optional shared segment-layout handle for segment-aware decoders.
    ///
    /// Segment-aware decoders (fMP4 segment demuxer) call this once at
    /// open to grab a lock-free, Arc-shareable view over the segment
    /// table — independent of the byte cursor passed to the decoder
    /// through `Read + Seek`. Default `None` for non-segmented sources.
    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        None
    }

    /// Optional HLS-only variant-coordination handle. Adaptive sources (HLS)
    /// return `Some`; non-adaptive sources keep the default `None`.
    fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
        None
    }

    /// Whether the source currently reports zero bytes. Default mirrors
    /// `self.len()` returning `0` (or being unknown — both are treated as
    /// "no readable bytes yet" for the conventional `len`/`is_empty` pair).
    fn is_empty(&self) -> bool {
        self.len().is_none_or(|n| n == 0)
    }

    /// Total length if known.
    ///
    /// Streaming sources may block briefly until the HTTP response headers
    /// arrive (Content-Length discovery).
    fn len(&self) -> Option<u64>;

    /// The reader→peer wake handle, if this source pushes a downloader peer.
    ///
    /// Segmented sources (HLS) return their [`DeferredWake`]; the driver
    /// reading or seeking the stream arms it on the produce core (the audio
    /// shell flushes it) or [`notify_now`](DeferredWake::notify_now)s it
    /// off-core, per the driver's own statically-known context. Non-segmented
    /// sources have no peer and return `None`.
    fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
        None
    }

    /// Get media info if available.
    fn media_info(&self) -> Option<MediaInfo> {
        None
    }

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
        let pos = self.position();
        self.phase_at(pos..pos.saturating_add(1))
    }

    /// Point-in-time snapshot of the source phase for the given range.
    ///
    /// Returns the current [`SourcePhase`] without blocking. Used internally
    /// by `wait_range()` implementations for fast-path dispatch.
    fn phase_at(&self, range: Range<u64>) -> SourcePhase;

    /// Current byte position in the source's virtual byte space.
    ///
    /// HLS delegates to active variant; file owns its own atomic cursor.
    fn position(&self) -> u64;

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

    /// Absolute set of the byte cursor — used by [`Stream::seek`] and
    /// post-seek landings. Sources implement this via the same atomic
    /// cursor that backs [`Self::position`] / [`Self::advance`].
    fn set_position(&self, pos: u64);

    /// Build a fresh reader-side event-sink instance.
    ///
    /// Returned by Source-impls that want to expose reader-side events
    /// (`HlsSource`, `FileSource`). The audio pipeline takes the sink
    /// at decoder creation/recreation time and threads it into the
    /// composed decoder. Default `None` keeps mock and test sources
    /// without a sink.
    ///
    /// Each call must return a **fresh** sink instance, because decoder
    /// recreation (ABR / format change) rebuilds the decoder and the new
    /// sink needs a clean state cursor.
    fn take_reader_event_sink(&mut self) -> Option<crate::BoxedEventSink> {
        None
    }

    /// Narrow read-only handle to the playhead position and total duration.
    fn playhead_read(&self) -> Arc<dyn PlayheadRead>;

    /// Narrow mutating handle to the playhead — for the decode/produce path.
    fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;

    /// Narrow read-only handle to seek/flush coordination state.
    fn seek_observe(&self) -> Arc<dyn SeekObserve>;

    /// Narrow mutating handle to seek coordination (`FLUSH_START` / `FLUSH_STOP`).
    fn seek_control(&self) -> Arc<dyn SeekControl>;

    /// Narrow handle to the playback-activity flag.
    fn activity(&self) -> Arc<dyn Activity>;

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
}

/// HLS-only variant-coordination surface, vended as `Some` by adaptive
/// sources and `None` by everything else. Replaces three former no-op
/// default `Source` methods — non-adaptive sources no longer pretend to
/// implement variant fences.
///
/// All methods take `&self`; the HLS impl (`HlsCoord`) uses interior
/// mutability, so callers hold an `Arc<dyn VariantControl>` and never
/// need `&mut`.
pub trait VariantControl: Send + Sync + 'static {
    /// Clear the variant fence after a decoder recreate acks an ABR switch.
    fn clear_variant_fence(&self);

    /// Whether a cross-variant transition is in-flight (reads/waits are
    /// short-circuited until the decoder acks via `clear_variant_fence`).
    fn has_variant_change_pending(&self) -> bool;

    /// Target variant index of the in-flight transition, `None` when no
    /// fence is pending. Published before the fence is raised, so any
    /// observer of a pending fence also sees the variant that fence
    /// demands. Lets the decoder ack a fence whose target it is already
    /// aligned with — no format diff will ever become observable there.
    fn variant_change_target(&self) -> Option<usize>;

    /// Byte range of the header the decoder must re-read after a format
    /// change (HLS ABR cross-codec switch).
    ///
    /// # Errors
    /// `Err(SourceError::FormatChangeNotApplicable)` when the active variant
    /// was served with a non-zero `served_from` so the init prefix lives
    /// outside the virtual range.
    fn format_change_segment_range(&self) -> StreamResult<Range<u64>>;
}

/// Segment-table view exposed by segmented sources (HLS, fragmented
/// file-mp4).
///
/// Carries the segment metadata that segment-aware decoders need to
/// route reads — `init_segment_range` (ftyp+moov / `EXT-X-MAP`),
/// `segment_at_time`, `segment_after_byte`, `segment_count`, and total
/// `len`. Has no I/O surface: the byte cursor is the decoder's
/// `Read + Seek` handle, queried independently. Sources that aren't
/// segment-aware return `None` from [`Source::byte_map`].
pub trait ByteMap: Send + Sync + 'static {
    /// Resolve `position` to a source-specific seek anchor (segment
    /// boundary + byte offset). Non-segmented / non-anchoring layouts
    /// keep the default `Ok(None)`.
    ///
    /// # Errors
    /// Returns an error when the layout cannot resolve the anchor.
    fn anchor_at_time(&self, _position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        Ok(None)
    }

    /// Init segment range (e.g. ftyp+moov from `EXT-X-MAP`) for the
    /// current layout variant. Returns an **empty** range (`0..0`) when
    /// the layout has no init segment (raw TS/AAC/MPEG-ES) or when the
    /// active variant has not yet announced one. Callers that require an
    /// init must check `Range::is_empty()` — distinguishing "no init"
    /// from "init at offset 0..0" is unsupported because every init we
    /// emit is non-empty by construction.
    fn init_segment_range(&self) -> Range<u64>;

    /// Whether the layout currently reports zero bytes. `len()` is `Option`
    /// because some segmented sources do not know their total upfront, so
    /// emptiness defaults to "len is `None` or `Some(0)`".
    fn is_empty(&self) -> bool {
        self.len().is_none_or(|n| n == 0)
    }

    /// Total byte length across all segments. Used to compute total
    /// duration when the source can't provide a direct value.
    fn len(&self) -> Option<u64>;

    /// Next segment whose byte range starts at or after `byte_offset`.
    /// Used for sequential play after the current segment is consumed.
    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor>;

    /// Segment whose `byte_range` covers `byte_offset`. Default `None`
    /// keeps non-segmented sources transparent.
    fn segment_at_byte(&self, _byte_offset: u64) -> Option<SegmentDescriptor> {
        None
    }

    /// Descriptor for the segment at `segment_index` in the current
    /// layout variant. Used by demuxers to re-resolve a cursor's
    /// `byte_range` against the live layout — without this, a DRM
    /// post-decrypt size shrink (PKCS7 padding stripped) between cursor
    /// setup and the actual read leaves `state.range` pointing past
    /// the segment's real end and `HlsSource::read_at` splices bytes
    /// from the next segment onto the buffer's tail. Returns `None`
    /// for non-segmented sources or for indices outside the current
    /// layout's range.
    fn segment_at_index(&self, _segment_index: u32) -> Option<SegmentDescriptor> {
        None
    }

    /// Locate the segment whose `[decode_time, decode_time + duration)`
    /// covers `t`. Resolves against the source's *current layout
    /// variant* — same variant `init_segment_range` describes.
    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor>;

    /// Total number of segments in the current layout variant.
    fn segment_count(&self) -> Option<u32>;
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{PlayheadState, SeekState};

    #[kithara::test]
    fn test_source_trait_object_safety() {
        fn _accepts_source<S: Source>(_s: S) {}
    }

    #[kithara::test]
    fn source_phase_defaults_to_waiting() {
        assert_eq!(SourcePhase::default(), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn phase_default_delegates_to_phase_at() {
        use std::sync::atomic::{AtomicU64, Ordering};

        struct ReadySource {
            seek: Arc<SeekState>,
            playhead: Arc<PlayheadState>,
            position: Arc<AtomicU64>,
        }
        impl Source for ReadySource {
            fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
            }
            fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
            }
            fn seek_observe(&self) -> Arc<dyn SeekObserve> {
                Arc::clone(&self.seek) as Arc<dyn SeekObserve>
            }
            fn seek_control(&self) -> Arc<dyn SeekControl> {
                Arc::clone(&self.seek) as Arc<dyn SeekControl>
            }
            fn activity(&self) -> Arc<dyn Activity> {
                Arc::clone(&self.seek) as Arc<dyn Activity>
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
            fn position(&self) -> u64 {
                self.position.load(Ordering::Acquire)
            }
            fn advance(&self, n: u64) {
                self.position.fetch_add(n, Ordering::AcqRel);
            }
            fn set_position(&self, pos: u64) {
                self.position.store(pos, Ordering::Release);
            }
        }
        let source = ReadySource {
            seek: Arc::new(SeekState::new()),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
        };
        assert_eq!(source.phase(), SourcePhase::Ready);
    }

    /// Exercises all five narrow `Source` accessor default methods.
    ///
    /// Verifies that:
    /// - `playhead_read().position()` starts at `Duration::ZERO`.
    /// - `playhead_read().duration()` starts at `None`.
    /// - `seek_observe().epoch()` starts at `0`.
    /// - `seek_control().begin(t)` returns a monotonically increasing epoch
    ///   and `seek_observe().epoch()` observes the new value.
    /// - `activity().is_playing()` toggles correctly.
    #[kithara::test]
    fn narrow_source_accessors_seam() {
        use std::sync::atomic::{AtomicU64, Ordering};

        struct MinimalSource {
            seek: Arc<SeekState>,
            playhead: Arc<PlayheadState>,
            position: Arc<AtomicU64>,
        }
        impl Source for MinimalSource {
            fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
            }
            fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
            }
            fn seek_observe(&self) -> Arc<dyn SeekObserve> {
                Arc::clone(&self.seek) as Arc<dyn SeekObserve>
            }
            fn seek_control(&self) -> Arc<dyn SeekControl> {
                Arc::clone(&self.seek) as Arc<dyn SeekControl>
            }
            fn activity(&self) -> Arc<dyn Activity> {
                Arc::clone(&self.seek) as Arc<dyn Activity>
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
                SourcePhase::Waiting
            }
            fn len(&self) -> Option<u64> {
                None
            }
            fn position(&self) -> u64 {
                self.position.load(Ordering::Acquire)
            }
            fn advance(&self, n: u64) {
                self.position.fetch_add(n, Ordering::AcqRel);
            }
            fn set_position(&self, pos: u64) {
                self.position.store(pos, Ordering::Release);
            }
        }

        let src = MinimalSource {
            seek: Arc::new(SeekState::new()),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
        };

        // playhead_read / playhead_write
        assert_eq!(src.playhead_read().position(), Duration::ZERO);
        assert_eq!(src.playhead_read().duration(), None);

        // seek_observe initial state
        assert_eq!(src.seek_observe().epoch(), 0);
        assert!(!src.seek_observe().is_flushing());
        assert!(src.seek_observe().target().is_none());

        // seek_control.begin bumps the epoch; seek_observe sees it
        let epoch = src.seek_control().begin(Duration::from_secs(5));
        assert_eq!(epoch, 1);
        assert_eq!(src.seek_observe().epoch(), 1);
        assert_eq!(src.seek_observe().target(), Some(Duration::from_secs(5)));
        assert!(src.seek_observe().is_flushing());

        // activity toggle
        assert!(!src.activity().is_playing());
        src.activity().set_playing(true);
        assert!(src.activity().is_playing());
        src.activity().set_playing(false);
        assert!(!src.activity().is_playing());
    }
}
