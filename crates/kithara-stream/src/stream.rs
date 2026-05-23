#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    fmt,
    future::Future,
    io::{self, Error as IoError, ErrorKind, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    ops::Range,
    sync::Arc,
};

use kithara_platform::{MaybeSend, MaybeSync, thread::yield_now, time::Duration, tokio::task};
use kithara_storage::WaitOutcome;
use kithara_test_utils::kithara;

/// Real error from [`Stream::try_read`] — the underlying source
/// surfaced an I/O failure.
///
/// Status conditions (seek pending, data not ready, variant change,
/// retry) are **not** errors and are carried in
/// [`StreamReadOutcome::Pending`] with a typed [`PendingReason`]. Only
/// genuine source failures end up here.
#[derive(Debug)]
#[non_exhaustive]
pub enum StreamReadError {
    /// Anything surfaced by the underlying [`Source`] as a real error.
    Source(IoError),
}

/// Outcome of a [`Stream::try_read`] call.
///
/// Mirrors the [`ReadOutcome`] shape from
/// [`Source::read_at`](crate::Source::read_at), but extends each variant
/// with the authoritative `byte_position` from the stream's
/// [`Timeline`] for callers that don't want to read it back themselves.
/// `Bytes` carries a [`NonZeroUsize`] count — the type system
/// guarantees forward progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReadOutcome {
    /// Stream produced `count` bytes. `byte_position` is the new byte
    /// offset **after** the read.
    Bytes {
        count: NonZeroUsize,
        byte_position: u64,
    },
    /// No progress this call. See [`PendingReason`] for the precise
    /// cause and required caller action.
    Pending(PendingReason),
    /// Natural end of stream. `byte_position` is the offset where EOF
    /// was observed (typically the source length).
    Eof { byte_position: u64 },
}

/// Typed error from [`Stream::seek`] for an absolute byte target that
/// lands beyond the stream's known length.
///
/// Surfaced as the typed payload of an `io::Error` (kind
/// [`ErrorKind::InvalidInput`]) so consumers like Symphonia preserve it
/// through their own error chain. Decoders downcast to recover the
/// structured info and classify the failure as caller-side (the seek
/// target is invalid for this stream, not a decoder state corruption).
#[derive(Debug, Clone, Copy)]
pub struct StreamSeekPastEof {
    pub current_pos: u64,
    pub len: u64,
    pub new_pos: u64,
}

impl fmt::Display for StreamSeekPastEof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "seek past EOF: new_pos={} len={} current_pos={}",
            self.new_pos, self.len, self.current_pos
        )
    }
}

impl StdError for StreamSeekPastEof {}

impl fmt::Display for StreamReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(e) => write!(f, "source error: {e}"),
        }
    }
}

impl StdError for StreamReadError {
    // ast-grep-ignore: idioms.match-self-conversion
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Source(e) => Some(e),
        }
    }
}

/// Typed payload of an `io::Error` (kind [`ErrorKind::Interrupted`])
/// emitted by `impl Read for Stream` when the underlying source could
/// not satisfy the read this call. Both `SeekPending` and
/// `NotReady`/`Retry` surface as `Interrupted` so demuxers (notably
/// Symphonia's fragmented MP4 reader) treat the pause as a transient
/// cooperative interruption and let `kithara-decode::is_seek_pending_io`
/// classify the failure correctly — the previous `WouldBlock` mapping
/// was treated as a hard "would block" by Symphonia's seek path and
/// corrupted the demuxer cursor on partial reads. Carries the
/// [`PendingReason`] verbatim plus a snapshot of source/timeline state
/// at the wrap site, so callers downcasting from `io::Error` recover
/// both *what* stalled and *why* without having to instrument their
/// own decoder.
#[derive(Debug, Clone, Copy)]
pub struct StreamPending {
    pub reason: PendingReason,
    pub pos: u64,
    pub want: usize,
    pub len: Option<u64>,
    pub phase: SourcePhase,
    pub epoch: u64,
    pub flushing: bool,
    pub variant_fence: bool,
}

impl fmt::Display for StreamPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: pos={} want={} len={:?} phase={:?} epoch={} flushing={} variant_fence={}",
            self.reason,
            self.pos,
            self.want,
            self.len,
            self.phase,
            self.epoch,
            self.flushing,
            self.variant_fence,
        )
    }
}

impl StdError for StreamPending {}

/// Non-retriable cross-variant boundary signal — the typed payload of
/// the `io::Error` produced by `impl Read for Stream` when the
/// underlying source fenced on a variant change. Decoders that go
/// through `std::io::Read` (Symphonia chain walker) downcast on this
/// type to recover the precise classification without string-matching.
#[derive(Debug, Clone, Copy)]
pub struct VariantChangeError;

impl fmt::Display for VariantChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("variant change: decoder recreation required")
    }
}

impl StdError for VariantChangeError {}

use crate::{
    MediaInfo, SourcePhase, SourceSeekAnchor, Timeline,
    error::{SourceError, StreamError},
    source::{NotReadyCause, PendingReason, ReadOutcome, Source},
};

/// Defines a stream type and how to create it.
///
/// This trait is implemented by marker types (`Hls`, `File`) in their respective crates.
/// The implementation provides the config type and source type.
///
/// On wasm32, `Send`/`Sync` bounds are relaxed via [`MaybeSend`]/[`MaybeSync`].
pub trait StreamType: MaybeSend + 'static {
    /// Configuration for this stream type.
    type Config: Default + MaybeSend;

    /// Event bus type carried by the stream config.
    ///
    /// Concrete stream types set this to `kithara_events::EventBus`.
    /// `Audio::new()` constrains `T::Events = EventBus` to extract it.
    type Events: Clone + MaybeSend + MaybeSync + 'static;

    /// Source implementing `Source`.
    type Source: Source;

    /// Create the source from configuration.
    ///
    /// May also start background tasks (downloader) internally.
    fn create(config: Self::Config) -> impl Future<Output = Result<Self::Source, SourceError>>;

    /// Extract the event bus from config (if set).
    ///
    /// Used by `Audio::new()` to share a single bus across the stream
    /// and the audio pipeline.
    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        let _ = config;
        None
    }
}

/// Generic audio stream with sync `Read + Seek`.
///
/// `T` is a marker type defining the stream source (`Hls`, `File`, etc.).
/// Stream holds the source directly and implements `Read + Seek` by calling
/// `Source::wait_range()` and `Source::read_at()`.
pub struct Stream<T: StreamType> {
    source: T::Source,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying stream source cannot be created.
    pub async fn new(config: T::Config) -> Result<Self, SourceError> {
        let source = T::create(config).await?;
        task::yield_now().await;
        Ok(Self { source })
    }

    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.source.position()
    }

    /// Resolve a deterministic time-based seek anchor.
    ///
    /// Returns `None` for sources without segmented time mapping.
    ///
    /// # Errors
    ///
    /// Returns an error when the source failed to resolve the anchor.
    pub fn seek_time_anchor(
        &mut self,
        position: Duration,
    ) -> Result<Option<SourceSeekAnchor>, io::Error> {
        self.source
            .seek_time_anchor(position)
            .map_err(|e| IoError::other(e.to_string()))
    }

    /// Get shared reference to inner source.
    pub fn source(&self) -> &T::Source {
        &self.source
    }

    /// Get stream timeline.
    pub fn timeline(&self) -> Timeline {
        self.source.timeline()
    }

    delegate::delegate! {
        to self.source {
            /// Overall source readiness at current position.
            pub fn phase(&self) -> SourcePhase;
            /// Point-in-time readiness for a specific byte range.
            pub fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            /// Get current media info if known.
            pub fn media_info(&self) -> Option<MediaInfo>;
            /// Runtime ABR handle — `Some` for adaptive sources (HLS).
            pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            /// Current variant metadata — `Some` for adaptive sources (HLS).
            pub fn current_variant(&self) -> Option<kithara_events::VariantInfo>;
            /// Get total length if known.
            pub fn len(&self) -> Option<u64>;
            /// Get current segment byte range (for segmented sources like HLS).
            /// Transitional — removed in Plan 06.
            pub fn current_segment_range(&self) -> Option<Range<u64>>;
            /// Header byte range for decoder recreate after a format change.
            /// Transitional — removed in Plan 06.
            ///
            /// # Errors
            ///
            /// `Err(SourceError::FormatChangeNotApplicable)` for non-HLS
            /// sources or HLS variants activated with `served_from > 0`
            /// (init prefix unreachable via Stream reads).
            pub fn format_change_segment_range(&self) -> crate::error::StreamResult<Range<u64>>;
            /// Clear variant fence, allowing reads from the next variant.
            pub fn clear_variant_fence(&mut self);
            /// `true` while a cross-variant fence keeps `read_at` / `wait_range`
            /// short-circuited to `Pending(VariantChange)` / `Interrupted`.
            pub fn has_variant_change_pending(&self) -> bool;
            /// Set seek epoch for stale request invalidation.
            pub fn set_seek_epoch(&mut self, seek_epoch: u64);
            /// Wake any blocked `wait_range()` calls.
            pub fn notify_waiting(&self);
            /// Create a lock-free callback for waking blocked `wait_range()`.
            pub fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>>;
            /// Commit the actual post-seek landing after `decoder.seek(...)`.
            pub fn commit_seek_landing(&mut self, anchor: Option<SourceSeekAnchor>);
            /// Build a fresh reader-side hooks instance from the inner source.
            pub fn take_reader_hooks(&mut self) -> Option<crate::SharedHooks>;
            /// Optional segment-layout handle for segment-aware decoders.
            pub fn as_segment_layout(&self) -> Option<Arc<dyn crate::SegmentLayout>>;
            /// Absolute byte-position set — used by [`Stream::seek`] callers
            /// and audio FSM landings. Forwards to the source's atomic cursor.
            pub fn set_position(&self, pos: u64);
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Maximum `wait_range` retries before returning
    /// `Pending(NotReady)` to the caller. Each retry takes
    /// `WAIT_RANGE_TIMEOUT` (10ms), so 50 iterations ≈ 500ms.
    /// Prevents the hang detector from firing when data is
    /// legitimately not yet available (e.g. encrypted HLS startup).
    const MAX_WAIT_SPINS: u32 = 50;

    /// `Seek` calls `wait_range(new_pos..new_pos+1)` to prime metadata
    /// (so `source.len()` answers before the seek-past-EOF check). A
    /// hard cap is required because a broken downloader would otherwise
    /// hang the seek forever; on the happy path local metadata resolves
    /// well under this budget. Matches the read path's total budget
    /// (`WAIT_RANGE_TIMEOUT * MAX_WAIT_SPINS`).
    const SEEK_WAIT_TIMEOUT: Duration = Duration::from_millis(500);

    /// Short timeout keeps the audio worker responsive for round-robin
    /// between tracks. At 44100Hz stereo with 4096-sample chunks, one chunk
    /// lasts ~46ms. A 10ms budget gives the worker time to serve other
    /// tracks and still refill the ringbuf before the audio callback drains it.
    const WAIT_RANGE_TIMEOUT: Duration = Duration::from_millis(10);

    /// Typed read — returns a [`StreamReadOutcome`] discriminating
    /// progress (`Bytes` with [`NonZeroUsize`]) from non-progress
    /// (`Pending` with a typed [`PendingReason`]) and natural EOF.
    /// Only genuine source I/O failures surface as
    /// [`StreamReadError::Source`]. `impl Read for Stream` wraps this
    /// outcome for `std::io::Read` consumers.
    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara::hang_watchdog]
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<StreamReadOutcome, StreamReadError> {
        if buf.is_empty() {
            return Ok(StreamReadOutcome::Eof {
                byte_position: self.source.position(),
            });
        }

        let mut wait_spins = 0u32;

        loop {
            let timeline = self.source.timeline();
            let read_epoch = timeline.seek_epoch();
            let pos = self.source.position();
            let range = pos..pos.saturating_add(buf.len() as u64);

            let wait_result = self
                .source
                .wait_range(range, Some(Self::WAIT_RANGE_TIMEOUT));
            let wait_outcome = match wait_result {
                Ok(outcome) => outcome,
                Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => {
                    if timeline.is_flushing() || timeline.seek_epoch() != read_epoch {
                        return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
                    }
                    wait_spins += 1;
                    if wait_spins >= Self::MAX_WAIT_SPINS {
                        return Ok(StreamReadOutcome::Pending(PendingReason::NotReady(
                            NotReadyCause::WaitBudgetExhausted,
                        )));
                    }
                    hang_tick!();
                    yield_now();
                    continue;
                }
                Err(e) => {
                    return Err(StreamReadError::Source(IoError::other(e.to_string())));
                }
            };
            match wait_outcome {
                WaitOutcome::Ready => {}
                WaitOutcome::Eof => {
                    return Ok(StreamReadOutcome::Eof { byte_position: pos });
                }
                WaitOutcome::Interrupted => {
                    if !timeline.is_flushing() {
                        wait_spins += 1;
                        if wait_spins >= Self::MAX_WAIT_SPINS {
                            return Ok(StreamReadOutcome::Pending(PendingReason::NotReady(
                                NotReadyCause::WaitInterrupted,
                            )));
                        }
                        hang_tick!();
                        yield_now();
                        continue;
                    }
                    return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
                }
            }

            wait_spins = 0;

            if timeline.seek_epoch() != read_epoch {
                return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
            }

            match self
                .source
                .read_at(pos, buf)
                .map_err(|e| StreamReadError::Source(IoError::other(e.to_string())))?
            {
                ReadOutcome::Bytes(count) => {
                    if timeline.seek_epoch() != read_epoch {
                        return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
                    }
                    hang_reset!();
                    timeline.set_segment_position(pos);
                    self.source.advance(count.get() as u64);
                    let new_pos = self.source.position();
                    return Ok(StreamReadOutcome::Bytes {
                        count,
                        byte_position: new_pos,
                    });
                }
                ReadOutcome::Eof => {
                    return Ok(StreamReadOutcome::Eof { byte_position: pos });
                }
                ReadOutcome::Pending(PendingReason::Retry) => {
                    hang_tick!();
                    yield_now();
                    continue;
                }
                ReadOutcome::Pending(reason) => {
                    return Ok(StreamReadOutcome::Pending(reason));
                }
            }
        }
    }
}

impl<T: StreamType> Read for Stream<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.try_read(buf) {
            Ok(StreamReadOutcome::Bytes { count, .. }) => Ok(count.get()),
            Ok(StreamReadOutcome::Eof { .. }) => Ok(0),
            Ok(StreamReadOutcome::Pending(reason @ PendingReason::SeekPending)) => {
                // `ErrorKind::Interrupted` is the in-band signal
                // `kithara-decode::is_seek_pending_io` checks for —
                // wrap the reason as the source for typed downcasts.
                Err(IoError::new(ErrorKind::Interrupted, reason))
            }
            Ok(StreamReadOutcome::Pending(
                reason @ (PendingReason::NotReady(_) | PendingReason::Retry),
            )) => {
                // Cooperative pause — the caller must retry once the
                // source has progressed (notify_waiting wakes them via
                // the Timeline's seek epoch or asset_store updates).
                // `ErrorKind::Interrupted` carries the same semantics
                // through Symphonia's seek chain as `SeekPending`,
                // letting `is_seek_pending_io` classify the failure as
                // transient. The previous `WouldBlock` mapping looked
                // like a hard "would block" to Symphonia's fmp4 reader,
                // which surfaced it as `SeekFailed("isomp4: invalid
                // atom size")` and corrupted the demuxer cursor on
                // HE-AAC v2 fragmented MP4 (observed as a false-EOF
                // cascade during rapid scrubs into cold byte ranges).
                Err(IoError::new(
                    ErrorKind::Interrupted,
                    self.snapshot_pending(reason, buf.len()),
                ))
            }
            Ok(StreamReadOutcome::Pending(PendingReason::VariantChange)) => {
                Err(IoError::other(VariantChangeError))
            }
            Err(StreamReadError::Source(e)) => Err(e),
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Build a typed [`StreamPending`] payload for a
    /// `Pending(NotReady|Retry)` surfaced through `impl Read`. Pulls
    /// live source/timeline state at the moment of the wrap so the
    /// resulting `io::Error` carries the real reason ("data not ready
    /// (`wait_budget_exhausted`): pos=N len=M phase=… epoch=E flushing=…
    /// `variant_fence`=…") instead of a bare "data not ready". Decoders
    /// downcast on `StreamPending` to recover the typed [`PendingReason`].
    fn snapshot_pending(&self, reason: PendingReason, want: usize) -> StreamPending {
        let pos = self.source.position();
        let len = self.source.len();
        let phase = self.source.phase_at(pos..pos.saturating_add(want as u64));
        let timeline = self.source.timeline();
        StreamPending {
            reason,
            pos,
            want,
            len,
            phase,
            epoch: timeline.seek_epoch(),
            flushing: timeline.is_flushing(),
            variant_fence: self.source.has_variant_change_pending(),
        }
    }
}

impl<T: StreamType> Seek for Stream<T> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let current = self.source.position();

        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::Current(delta) => i128::from(current).saturating_add(i128::from(delta)),
            SeekFrom::End(delta) => {
                if self.source.len().is_none() {
                    let _ = self.source.wait_range(0..1, None);
                }
                let Some(len) = self.source.len() else {
                    return Err(IoError::new(
                        ErrorKind::Unsupported,
                        "seek from end requires known length",
                    ));
                };
                i128::from(len).saturating_add(i128::from(delta))
            }
        };

        if new_pos < 0 {
            return Err(IoError::new(
                ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        let new_pos = u64::try_from(new_pos).unwrap_or(u64::MAX);

        let wait_range = match self.source.format_change_segment_range() {
            Ok(range) if range.start == new_pos => range,
            _ => new_pos..new_pos.saturating_add(1),
        };
        let _ = self
            .source
            .wait_range(wait_range, Some(Self::SEEK_WAIT_TIMEOUT));

        if let Some(len) = self.source.len()
            && new_pos > len
        {
            return Err(IoError::new(
                ErrorKind::InvalidInput,
                StreamSeekPastEof {
                    new_pos,
                    len,
                    current_pos: current,
                },
            ));
        }

        self.source.set_position(new_pos);
        Ok(new_pos)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use kithara_storage::WaitOutcome;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{ReadOutcome, Source, SourcePhase};

    /// Test helper — script entry that maps to either `Bytes(N)` (with
    /// the source slicing actual `data`) or a terminal `Eof`. Pending
    /// causes are exercised through the timeline (`initiate_seek`) and
    /// the wait-outcome script, not the read script.
    #[derive(Clone, Copy)]
    enum ScriptRead {
        Data(usize),
        Eof,
    }

    fn bytes(count: usize) -> ReadOutcome {
        let nz = NonZeroUsize::new(count)
            .expect("BUG: ScriptSource::bytes invariant — count must be > 0");
        ReadOutcome::Bytes(nz)
    }

    struct ScriptSource {
        position: Arc<AtomicU64>,
        anchor: Option<SourceSeekAnchor>,
        timeline: Timeline,
        data: Vec<u8>,
        reads: VecDeque<ScriptRead>,
        waits: VecDeque<WaitOutcome>,
    }

    impl ScriptSource {
        fn new(
            timeline: Timeline,
            waits: impl IntoIterator<Item = WaitOutcome>,
            reads: impl IntoIterator<Item = ScriptRead>,
            data: Vec<u8>,
        ) -> Self {
            Self {
                timeline,
                data,
                position: Arc::new(AtomicU64::new(0)),
                anchor: None,
                reads: reads.into_iter().collect(),
                waits: waits.into_iter().collect(),
            }
        }
    }

    impl Source for ScriptSource {
        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn len(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Waiting
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> crate::StreamResult<ReadOutcome> {
            let step = self.reads.pop_front().unwrap_or(ScriptRead::Eof);
            match step {
                ScriptRead::Eof => Ok(ReadOutcome::Eof),
                ScriptRead::Data(n) => {
                    let Ok(start) = usize::try_from(offset) else {
                        return Ok(ReadOutcome::Eof);
                    };
                    let end = (start + n).min(self.data.len());
                    let bytes_count = end.saturating_sub(start).min(buf.len());
                    if bytes_count == 0 {
                        return Ok(ReadOutcome::Eof);
                    }
                    buf[..bytes_count].copy_from_slice(&self.data[start..start + bytes_count]);
                    Ok(bytes(bytes_count))
                }
            }
        }

        fn seek_time_anchor(
            &mut self,
            _position: Duration,
        ) -> crate::StreamResult<Option<SourceSeekAnchor>> {
            Ok(self.anchor)
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn timeline(&self) -> Timeline {
            self.timeline.clone()
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> crate::StreamResult<WaitOutcome> {
            Ok(self.waits.pop_front().unwrap_or(WaitOutcome::Ready))
        }
    }

    struct DummyType;

    impl StreamType for DummyType {
        type Config = ();
        type Events = ();
        type Source = ScriptSource;

        async fn create(_config: Self::Config) -> Result<Self::Source, SourceError> {
            Err(SourceError::other(IoError::other("not used in unit tests")))
        }
    }

    struct SeekDuringWaitType;

    impl StreamType for SeekDuringWaitType {
        type Config = ();
        type Events = ();
        type Source = SeekDuringWaitSource;

        async fn create(_config: Self::Config) -> Result<Self::Source, SourceError> {
            Err(SourceError::other(IoError::other("not used in unit tests")))
        }
    }

    struct SeekDuringWaitSource {
        position: Arc<AtomicU64>,
        timeline: Timeline,
        read_calls: usize,
    }

    impl Source for SeekDuringWaitSource {
        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn len(&self) -> Option<u64> {
            Some(4)
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Ready
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> crate::StreamResult<ReadOutcome> {
            self.read_calls += 1;
            Ok(bytes(4))
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn timeline(&self) -> Timeline {
            self.timeline.clone()
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> crate::StreamResult<WaitOutcome> {
            let _ = self.timeline.initiate_seek(Duration::from_millis(10));
            Ok(WaitOutcome::Ready)
        }
    }

    #[kithara::test]
    fn read_retries_interrupted_when_not_flushing() {
        let timeline = Timeline::new();
        let source = ScriptSource::new(
            timeline.clone(),
            [WaitOutcome::Interrupted, WaitOutcome::Ready],
            [ScriptRead::Data(4)],
            b"ABCD".to_vec(),
        );
        let mut stream = Stream::<DummyType> { source };
        let mut buf = [0u8; 4];

        let n = stream
            .read(&mut buf)
            .expect("BUG: read must succeed after the explicit retry in this test scenario");
        assert_eq!(n, 4);
        assert_eq!(&buf, b"ABCD");
    }

    #[kithara::test]
    fn try_read_returns_seek_pending_when_flushing() {
        let timeline = Timeline::new();
        let _ = timeline.initiate_seek(Duration::from_millis(10));
        let source = ScriptSource::new(timeline.clone(), [WaitOutcome::Interrupted], [], vec![]);
        let mut stream = Stream::<DummyType> { source };
        let mut buf = [0u8; 4];

        let outcome = stream
            .try_read(&mut buf)
            .expect("BUG: seek-pending is a status return; not a hard error in this test");
        assert!(matches!(
            outcome,
            StreamReadOutcome::Pending(PendingReason::SeekPending)
        ));
    }

    #[kithara::test]
    fn try_read_returns_seek_pending_when_epoch_changes_after_wait() {
        let timeline = Timeline::new();
        let source = SeekDuringWaitSource {
            timeline: timeline.clone(),
            position: Arc::new(AtomicU64::new(0)),
            read_calls: 0,
        };
        let mut stream = Stream::<SeekDuringWaitType> { source };
        let mut buf = [0u8; 4];

        let outcome = stream
            .try_read(&mut buf)
            .expect("BUG: seek-pending is a status return; not a hard error in this test");

        assert!(matches!(
            outcome,
            StreamReadOutcome::Pending(PendingReason::SeekPending)
        ));
        assert_eq!(stream.source.read_calls, 0);
        assert_eq!(stream.position(), 0);
    }

    #[kithara::test]
    fn seek_updates_position() {
        let timeline = Timeline::new();
        let source = ScriptSource::new(timeline.clone(), [], [], b"ABCDE".to_vec());
        let mut stream = Stream::<DummyType> { source };

        let pos = stream
            .seek(SeekFrom::Start(3))
            .expect("BUG: seek to a position within the test stream must succeed");

        assert_eq!(pos, 3);
        assert_eq!(stream.position(), 3);
    }

    #[kithara::test]
    fn seek_time_anchor_does_not_move_position() {
        let timeline = Timeline::new();
        let mut source = ScriptSource::new(timeline.clone(), [], [], b"ABCDE".to_vec());
        source.set_position(11);
        source.anchor = Some(SourceSeekAnchor {
            byte_offset: 3,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(2),
            variant_index: Some(1),
        });
        let mut stream = Stream::<DummyType> { source };

        let anchor = stream
            .seek_time_anchor(Duration::from_millis(8_500))
            .expect("BUG: anchor resolution must succeed for the constructed test stream")
            .expect("BUG: stream must return the resolved anchor in this test");

        assert_eq!(anchor.byte_offset, 3);
        assert_eq!(
            stream.position(),
            11,
            "anchor resolution must not eagerly commit stream position"
        );
    }
}
