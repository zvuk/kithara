//! Audio stream types and traits.
//!
//! Provides `Stream<T>` - a generic audio stream parameterized by stream type.
//!
//! Marker types (`Hls`, `File`) are defined in their respective crates
//! and implement `StreamType` trait.

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
    pub new_pos: u64,
    pub len: u64,
    pub current_pos: u64,
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
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Source(e) => Some(e),
        }
    }
}

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
    MediaInfo, SourcePhase, SourceSeekAnchor, StreamContext, Timeline,
    error::SourceError,
    source::{PendingReason, ReadOutcome, Source},
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

    /// Source implementing `Source`.
    type Source: Source;

    /// Create the source from configuration.
    ///
    /// May also start background tasks (downloader) internally.
    fn create(config: Self::Config) -> impl Future<Output = Result<Self::Source, SourceError>>;

    /// Event bus type carried by the stream config.
    ///
    /// Concrete stream types set this to `kithara_events::EventBus`.
    /// `Audio::new()` constrains `T::Events = EventBus` to extract it.
    type Events: Clone + MaybeSend + MaybeSync + 'static;

    /// Extract the event bus from config (if set).
    ///
    /// Used by `Audio::new()` to share a single bus across the stream
    /// and the audio pipeline.
    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        let _ = config;
        None
    }

    /// Build a `StreamContext` from the source and shared byte position.
    ///
    /// Default returns `NullStreamContext` (no segment/variant info).
    /// HLS overrides with `HlsStreamContext` carrying segment/variant atomics.
    fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(crate::NullStreamContext::new(timeline))
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
        // Yield so background tasks (Downloader loop) spawned during
        // create() get a chance to start on current-thread runtimes.
        task::yield_now().await;
        Ok(Self { source })
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.source.timeline().byte_position()
    }

    /// Get stream timeline.
    pub fn timeline(&self) -> Timeline {
        self.source.timeline()
    }

    /// Get shared reference to inner source.
    pub fn source(&self) -> &T::Source {
        &self.source
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
            /// Get total length if known.
            pub fn len(&self) -> Option<u64>;
            /// Get current segment byte range (for segmented sources like HLS).
            pub fn current_segment_range(&self) -> Option<Range<u64>>;
            /// Get byte range of first segment with current format after ABR switch.
            pub fn format_change_segment_range(&self) -> Option<Range<u64>>;
            /// Clear variant fence, allowing reads from the next variant.
            pub fn clear_variant_fence(&mut self);
            /// Switch layout to ABR target variant before decoder recreation.
            pub fn commit_variant_layout(&mut self);
            /// Set seek epoch for stale request invalidation.
            pub fn set_seek_epoch(&mut self, seek_epoch: u64);
            /// Signal that the given byte range will be needed soon.
            pub fn demand_range(&self, range: Range<u64>);
            /// Wake any blocked `wait_range()` calls.
            pub fn notify_waiting(&self);
            /// Create a lock-free callback for waking blocked `wait_range()`.
            pub fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>>;
            /// Commit the actual post-seek landing after `decoder.seek(...)`.
            pub fn commit_seek_landing(&mut self, anchor: Option<SourceSeekAnchor>);
            /// Build a fresh reader-side hooks instance from the inner source.
            pub fn take_reader_hooks(&mut self) -> Option<crate::SharedHooks>;
            /// Init segment range (HLS only).
            pub fn init_segment_range(&self) -> Option<Range<u64>>;
            /// Map a target time to a segment descriptor.
            pub fn segment_at_time(&self, t: Duration) -> Option<crate::SegmentDescriptor>;
            /// Next segment whose byte range starts at or after `byte_offset`.
            pub fn segment_after_byte(&self, byte_offset: u64) -> Option<crate::SegmentDescriptor>;
            /// Total number of segments in the current layout variant.
            pub fn segment_count(&self) -> Option<u32>;
            /// Optional shared handle exposing per-segment metadata.
            pub fn as_segment_source(&self) -> Option<Arc<dyn Source>>;
        }
    }

    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
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
}

impl<T: StreamType> Stream<T> {
    /// Typed read — returns a [`StreamReadOutcome`] discriminating
    /// progress (`Bytes` with [`NonZeroUsize`]) from non-progress
    /// (`Pending` with a typed [`PendingReason`]) and natural EOF.
    /// Only genuine source I/O failures surface as
    /// [`StreamReadError::Source`]. `impl Read for Stream` wraps this
    /// outcome for `std::io::Read` consumers.
    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara_hang_detector::hang_watchdog]
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<StreamReadOutcome, StreamReadError> {
        /// Short timeout keeps the audio worker responsive for round-robin
        /// between tracks. At 44100Hz stereo with 4096-sample chunks, one chunk
        /// lasts ~46ms. A 10ms budget gives the worker time to serve other
        /// tracks and still refill the ringbuf before the audio callback drains it.
        const WAIT_RANGE_TIMEOUT: Duration = Duration::from_millis(10);

        /// Maximum `wait_range` retries before returning
        /// `Pending(NotReady)` to the caller. Each retry takes
        /// `WAIT_RANGE_TIMEOUT` (10ms), so 50 iterations ≈ 500ms.
        /// Prevents the hang detector from firing when data is
        /// legitimately not yet available (e.g. encrypted HLS startup).
        const MAX_WAIT_SPINS: u32 = 50;

        let timeline = self.source.timeline();

        if buf.is_empty() {
            return Ok(StreamReadOutcome::Eof {
                byte_position: timeline.byte_position(),
            });
        }

        let mut wait_spins = 0u32;

        loop {
            let timeline = self.source.timeline();
            let read_epoch = timeline.seek_epoch();
            let pos = timeline.byte_position();
            let range = pos..pos.saturating_add(buf.len() as u64);

            let wait_result = self.source.wait_range(range, Some(WAIT_RANGE_TIMEOUT));
            let wait_outcome = match wait_result {
                Ok(outcome) => outcome,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("budget exceeded") {
                        if timeline.is_flushing() || timeline.seek_epoch() != read_epoch {
                            return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
                        }
                        wait_spins += 1;
                        if wait_spins >= MAX_WAIT_SPINS {
                            return Ok(StreamReadOutcome::Pending(PendingReason::NotReady));
                        }
                        hang_tick!();
                        yield_now();
                        continue;
                    }
                    return Err(StreamReadError::Source(IoError::other(msg)));
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
                        if wait_spins >= MAX_WAIT_SPINS {
                            return Ok(StreamReadOutcome::Pending(PendingReason::NotReady));
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
                    let new_pos = pos.saturating_add(count.get() as u64);
                    timeline.set_byte_position(new_pos);
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
            // `Pending(SeekPending)` rides as the typed inner of
            // `IoError::other` so consumers of the `io::Read` surface
            // (Symphonia chain walker in `kithara-decode`) can
            // `downcast_ref` it without matching on strings.
            Ok(StreamReadOutcome::Pending(reason @ PendingReason::SeekPending)) => {
                Err(IoError::other(reason))
            }
            Ok(StreamReadOutcome::Pending(PendingReason::NotReady | PendingReason::Retry)) => {
                Err(IoError::new(ErrorKind::Interrupted, "data not ready"))
            }
            Ok(StreamReadOutcome::Pending(PendingReason::VariantChange)) => {
                Err(IoError::other(VariantChangeError))
            }
            Err(StreamReadError::Source(e)) => Err(e),
        }
    }
}

impl<T: StreamType> Seek for Stream<T> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let timeline = self.source.timeline();
        let current = timeline.byte_position();

        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::Current(delta) => i128::from(current).saturating_add(i128::from(delta)),
            SeekFrom::End(delta) => {
                if self.source.len().is_none() {
                    // Wait until the source learns its length OR is
                    // cancelled / superseded by a new seek epoch — see
                    // [`Source::wait_range`] for the cancellation contract.
                    // No wall-clock budget: a slow connection must not
                    // make seek silently give up.
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

        #[expect(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let new_pos = new_pos as u64;

        // Wait for the target byte unboundedly; only `coord.cancel` (track
        // replaced / resource dropped / shutdown) or `seek_epoch` advance
        // (the user issued another seek) interrupts the wait. The previous
        // 10 s wall-clock budget caused position-frozen-at-target hangs on
        // slow connections — `red`-pinned by
        // `hls_seek_middle_lands_under_simulated_slow_connection`.
        let _ = self
            .source
            .wait_range(new_pos..new_pos.saturating_add(1), None);

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

        timeline.set_byte_position(new_pos);
        Ok(new_pos)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use kithara_storage::WaitOutcome;

    use super::*;
    use crate::{ReadOutcome, Source, SourcePhase};

    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

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
        let nz = NonZeroUsize::new(count).expect("ScriptSource::bytes: count must be > 0");
        ReadOutcome::Bytes(nz)
    }

    struct ScriptSource {
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
                anchor: None,
                timeline,
                data,
                reads: reads.into_iter().collect(),
                waits: waits.into_iter().collect(),
            }
        }
    }

    impl Source for ScriptSource {
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

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Waiting
        }

        fn len(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }

        fn seek_time_anchor(
            &mut self,
            _position: Duration,
        ) -> crate::StreamResult<Option<SourceSeekAnchor>> {
            Ok(self.anchor)
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
        timeline: Timeline,
        read_calls: usize,
    }

    impl Source for SeekDuringWaitSource {
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

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> crate::StreamResult<ReadOutcome> {
            self.read_calls += 1;
            Ok(bytes(4))
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Ready
        }

        fn len(&self) -> Option<u64> {
            Some(4)
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
            .expect("read must succeed after retry");
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
            .expect("seek-pending is a status, not an error");
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
            read_calls: 0,
        };
        let mut stream = Stream::<SeekDuringWaitType> { source };
        let mut buf = [0u8; 4];

        let outcome = stream
            .try_read(&mut buf)
            .expect("seek-pending is a status, not an error");

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

        let pos = stream.seek(SeekFrom::Start(3)).expect("seek must succeed");

        assert_eq!(pos, 3);
        assert_eq!(stream.position(), 3);
    }

    #[kithara::test]
    fn seek_time_anchor_does_not_move_position() {
        let timeline = Timeline::new();
        timeline.set_byte_position(11);
        let mut source = ScriptSource::new(timeline.clone(), [], [], b"ABCDE".to_vec());
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
            .expect("anchor resolution should succeed")
            .expect("stream should return the resolved anchor");

        assert_eq!(anchor.byte_offset, 3);
        assert_eq!(
            stream.position(),
            11,
            "anchor resolution must not eagerly commit stream position"
        );
    }
}
