//! Audio stream types and traits.
//!
//! Provides `Stream<T>` - a generic audio stream parameterized by stream type.
//!
//! Marker types (`Hls`, `File`) are defined in their respective crates
//! and implement `StreamType` trait.

#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    future::Future,
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use kithara_platform::{MaybeSend, MaybeSync, time::Duration};
use kithara_storage::WaitOutcome;

use crate::{
    MediaInfo, SourcePhase, SourceSeekAnchor, StreamContext, Timeline,
    coordination::TransferCoordination,
    layout::LayoutIndex,
    source::{ReadOutcome, Source, VariantChangeError},
    topology::Topology,
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
    /// Read-only media structure for this stream type.
    type Topology: Topology;
    /// Committed placement of logical items in the virtual byte space.
    type Layout: LayoutIndex;
    /// Shared runtime coordination between source and downloader.
    type Coord: TransferCoordination<Self::Demand>;
    /// On-demand request type used by the stream-specific coordinator.
    type Demand: Clone + Send + Sync + 'static;

    /// Source implementing `Source`.
    type Source: Source<
            Topology = Self::Topology,
            Layout = Self::Layout,
            Coord = Self::Coord,
            Demand = Self::Demand,
        >;

    /// Error type for stream creation.
    type Error: StdError + Send + Sync + 'static;

    /// Create the source from configuration.
    ///
    /// May also start background tasks (downloader) internally.
    fn create(config: Self::Config) -> impl Future<Output = Result<Self::Source, Self::Error>>;

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
    pub async fn new(config: T::Config) -> Result<Self, T::Error> {
        let source = T::create(config).await?;
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
            /// Get total length if known.
            pub fn len(&self) -> Option<u64>;
            /// Get current segment byte range (for segmented sources like HLS).
            pub fn current_segment_range(&self) -> Option<Range<u64>>;
            /// Get byte range of first segment with current format after ABR switch.
            pub fn format_change_segment_range(&self) -> Option<Range<u64>>;
            /// Clear variant fence, allowing reads from the next variant.
            pub fn clear_variant_fence(&mut self);
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
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

const WAIT_RANGE_TIMEOUT: Duration = Duration::from_secs(10);

impl<T: StreamType> Read for Stream<T> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara_hang_detector::hang_watchdog]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Retry loop: ReadOutcome::Retry means a resource was evicted between
        // wait_range (metadata ready) and read_at (actual I/O). Go back to
        // wait_range so the downloader can re-fetch.
        loop {
            let timeline = self.source.timeline();
            let read_epoch = timeline.seek_epoch();
            let pos = timeline.byte_position();
            let range = pos..pos.saturating_add(buf.len() as u64);

            // Wait for data to be available (blocking)
            match self
                .source
                .wait_range(range, WAIT_RANGE_TIMEOUT)
                .map_err(|e| io::Error::other(e.to_string()))?
            {
                WaitOutcome::Ready => {}
                WaitOutcome::Eof => return Ok(0),
                WaitOutcome::Interrupted => {
                    if !timeline.is_flushing() {
                        // Some sources use Interrupted as a recoverable
                        // "retry wait_range" signal when seek is not active.
                        hang_tick!();
                        kithara_platform::thread::yield_now();
                        continue;
                    }
                    // Use `Other` instead of `Interrupted` because Symphonia
                    // silently retries `Interrupted` errors (standard Rust I/O
                    // convention).  `Other` propagates through the decoder and
                    // becomes `DecodeError::Io`, which `handle_decode_error`
                    // can recover from by exiting to the worker loop.
                    return Err(io::Error::other("seek pending"));
                }
            }

            if timeline.seek_epoch() != read_epoch {
                return Err(io::Error::other("seek pending"));
            }

            // Read data directly from source.
            // No flushing check here: wait_range already handles interruption,
            // and the decoder must be able to read during apply_pending_seek().
            match self
                .source
                .read_at(pos, buf)
                .map_err(|e| io::Error::other(e.to_string()))?
            {
                ReadOutcome::Data(n) => {
                    if timeline.seek_epoch() != read_epoch {
                        return Err(io::Error::other("seek pending"));
                    }
                    hang_reset!();
                    timeline.set_byte_position(pos.saturating_add(n as u64));
                    return Ok(n);
                }
                ReadOutcome::VariantChange => {
                    return Err(io::Error::other(VariantChangeError));
                }
                ReadOutcome::Retry => {
                    // Resource evicted — go back to wait_range.
                    hang_tick!();
                    kithara_platform::thread::yield_now();
                    continue;
                }
            }
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
                let Some(len) = self.source.len() else {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "seek from end requires known length",
                    ));
                };
                i128::from(len).saturating_add(i128::from(delta))
            }
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        #[expect(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        // new_pos is verified non-negative above; i128 to u64 is safe after bounds check
        let new_pos = new_pos as u64;

        if let Some(len) = self.source.len()
            && new_pos > len
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "seek past EOF: new_pos={new_pos} len={len} current_pos={current} seek_from={pos:?}",
                ),
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
    use crate::{DemandSlot, ReadOutcome, Source, SourcePhase};

    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

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

    struct ScriptSource {
        anchor: Option<SourceSeekAnchor>,
        coord: TestCoord,
        data: Vec<u8>,
        reads: VecDeque<ReadOutcome>,
        waits: VecDeque<WaitOutcome>,
    }

    impl ScriptSource {
        fn new(
            timeline: Timeline,
            waits: impl IntoIterator<Item = WaitOutcome>,
            reads: impl IntoIterator<Item = ReadOutcome>,
            data: Vec<u8>,
        ) -> Self {
            Self {
                anchor: None,
                coord: TestCoord {
                    demand: DemandSlot::new(),
                    timeline,
                },
                data,
                reads: reads.into_iter().collect(),
                waits: waits.into_iter().collect(),
            }
        }
    }

    impl Source for ScriptSource {
        type Error = io::Error;
        type Topology = ();
        type Layout = ();
        type Coord = TestCoord;
        type Demand = ();

        fn topology(&self) -> &Self::Topology {
            &()
        }

        fn layout(&self) -> &Self::Layout {
            &()
        }

        fn coord(&self) -> &Self::Coord {
            &self.coord
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Duration,
        ) -> crate::StreamResult<WaitOutcome, Self::Error> {
            Ok(self.waits.pop_front().unwrap_or(WaitOutcome::Ready))
        }

        fn read_at(
            &mut self,
            offset: u64,
            buf: &mut [u8],
        ) -> crate::StreamResult<ReadOutcome, Self::Error> {
            let outcome = self.reads.pop_front().unwrap_or(ReadOutcome::Data(0));
            if let ReadOutcome::Data(n) = outcome {
                let start = offset as usize;
                let end = (start + n).min(self.data.len());
                let bytes = end.saturating_sub(start).min(buf.len());
                if bytes > 0 {
                    buf[..bytes].copy_from_slice(&self.data[start..start + bytes]);
                }
                return Ok(ReadOutcome::Data(bytes));
            }
            Ok(outcome)
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
        ) -> crate::StreamResult<Option<SourceSeekAnchor>, Self::Error> {
            Ok(self.anchor)
        }
    }

    struct DummyType;

    impl StreamType for DummyType {
        type Config = ();
        type Coord = TestCoord;
        type Demand = ();
        type Error = io::Error;
        type Events = ();
        type Layout = ();
        type Source = ScriptSource;
        type Topology = ();

        async fn create(_config: Self::Config) -> Result<Self::Source, Self::Error> {
            Err(io::Error::other("not used in unit tests"))
        }
    }

    struct SeekDuringWaitType;

    impl StreamType for SeekDuringWaitType {
        type Config = ();
        type Coord = TestCoord;
        type Demand = ();
        type Error = io::Error;
        type Events = ();
        type Layout = ();
        type Source = SeekDuringWaitSource;
        type Topology = ();

        async fn create(_config: Self::Config) -> Result<Self::Source, Self::Error> {
            Err(io::Error::other("not used in unit tests"))
        }
    }

    struct SeekDuringWaitSource {
        coord: TestCoord,
        read_calls: usize,
    }

    impl Source for SeekDuringWaitSource {
        type Error = io::Error;
        type Topology = ();
        type Layout = ();
        type Coord = TestCoord;
        type Demand = ();

        fn topology(&self) -> &Self::Topology {
            &()
        }

        fn layout(&self) -> &Self::Layout {
            &()
        }

        fn coord(&self) -> &Self::Coord {
            &self.coord
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Duration,
        ) -> crate::StreamResult<WaitOutcome, Self::Error> {
            let _ = self.coord.timeline.initiate_seek(Duration::from_millis(10));
            Ok(WaitOutcome::Ready)
        }

        fn read_at(
            &mut self,
            _offset: u64,
            _buf: &mut [u8],
        ) -> crate::StreamResult<ReadOutcome, Self::Error> {
            self.read_calls += 1;
            Ok(ReadOutcome::Data(4))
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
            [ReadOutcome::Data(4)],
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
    fn read_propagates_interrupted_when_flushing() {
        let timeline = Timeline::new();
        let _ = timeline.initiate_seek(Duration::from_millis(10));
        let source = ScriptSource::new(timeline.clone(), [WaitOutcome::Interrupted], [], vec![]);
        let mut stream = Stream::<DummyType> { source };
        let mut buf = [0u8; 4];

        let err = stream
            .read(&mut buf)
            .expect_err("flushing read must return error");
        // Uses `Other` (not `Interrupted`) so that Symphonia propagates
        // the error instead of silently retrying.
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[kithara::test]
    fn read_aborts_when_seek_epoch_changes_after_wait() {
        let timeline = Timeline::new();
        let source = SeekDuringWaitSource {
            coord: TestCoord {
                demand: DemandSlot::new(),
                timeline: timeline.clone(),
            },
            read_calls: 0,
        };
        let mut stream = Stream::<SeekDuringWaitType> { source };
        let mut buf = [0u8; 4];

        let err = stream
            .read(&mut buf)
            .expect_err("seek epoch change must abort stale read");

        assert_eq!(err.kind(), io::ErrorKind::Other);
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
