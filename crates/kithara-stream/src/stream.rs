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
    MediaInfo, SourceSeekAnchor, StreamContext, Timeline,
    source::{ReadOutcome, Source},
};

const WAIT_RANGE_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// HLS returns `HlsStreamContext` with segment/variant atomics.
    /// File returns `NullStreamContext` (no segment/variant).
    fn build_stream_context(source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext>;
}

/// Generic audio stream with sync `Read + Seek`.
///
/// `T` is a marker type defining the stream source (`Hls`, `File`, etc.).
/// Stream holds the source directly and implements `Read + Seek` by calling
/// `Source::wait_range()` and `Source::read_at()`.
pub struct Stream<T: StreamType> {
    source: T::Source,
    timeline: Timeline,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying stream source cannot be created.
    pub async fn new(config: T::Config) -> Result<Self, T::Error> {
        let source = T::create(config).await?;
        let timeline = source.timeline();
        Ok(Self { source, timeline })
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.timeline.byte_position()
    }

    /// Get stream timeline.
    pub fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    /// Get shared reference to inner source.
    pub fn source(&self) -> &T::Source {
        &self.source
    }

    /// Get current media info if known.
    pub fn media_info(&self) -> Option<MediaInfo> {
        self.source.media_info()
    }

    /// Get total length if known.
    #[expect(clippy::len_without_is_empty)]
    pub fn len(&self) -> Option<u64> {
        self.source.len()
    }

    /// Get current segment byte range (for segmented sources like HLS).
    pub fn current_segment_range(&self) -> Option<Range<u64>> {
        self.source.current_segment_range()
    }

    /// Get byte range of first segment with current format after ABR switch.
    ///
    /// For HLS: returns the first segment of the new variant which contains
    /// init data (ftyp/moov). This is where the decoder should be recreated.
    pub fn format_change_segment_range(&self) -> Option<Range<u64>> {
        self.source.format_change_segment_range()
    }

    /// Clear variant fence, allowing reads from the next variant.
    pub fn clear_variant_fence(&mut self) {
        self.source.clear_variant_fence();
    }

    /// Set seek epoch for stale request invalidation.
    pub fn set_seek_epoch(&mut self, seek_epoch: u64) {
        self.source.set_seek_epoch(seek_epoch);
    }

    /// Wake any blocked `wait_range()` calls.
    ///
    /// Called after `Timeline::initiate_seek()` for instant wakeup.
    pub fn notify_waiting(&self) {
        self.source.notify_waiting();
    }

    /// Create a lock-free callback for waking blocked `wait_range()`.
    ///
    /// The returned closure captures only the condvar/notify primitive,
    /// so it can be called without holding the `SharedStream` mutex.
    pub fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        self.source.make_notify_fn()
    }

    /// Resolve a deterministic time-based seek anchor and move the stream position.
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
        let anchor = self
            .source
            .seek_time_anchor(position)
            .map_err(|e| io::Error::other(e.to_string()))?;
        if let Some(anchor) = anchor {
            self.timeline.set_byte_position(anchor.byte_offset);
        }
        Ok(anchor)
    }
}

impl<T: StreamType> Read for Stream<T> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Retry loop: ReadOutcome::Retry means a resource was evicted between
        // wait_range (metadata ready) and read_at (actual I/O). Go back to
        // wait_range so the downloader can re-fetch.
        kithara_platform::hang_watchdog! {
            thread: "stream.read";
            loop {
                let pos = self.timeline.byte_position();
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
                        if !self.timeline.is_flushing() {
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

                // Read data directly from source.
                // No flushing check here: wait_range already handles interruption,
                // and the decoder must be able to read during apply_pending_seek().
                match self
                    .source
                    .read_at(pos, buf)
                    .map_err(|e| io::Error::other(e.to_string()))?
                {
                    ReadOutcome::Data(n) => {
                        hang_reset!();
                        self.timeline
                            .set_byte_position(pos.saturating_add(n as u64));
                        return Ok(n);
                    }
                    ReadOutcome::VariantChange => {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "variant change: decoder recreation required",
                        ));
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
}

impl<T: StreamType> Seek for Stream<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let current = self.timeline.byte_position();
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

        self.timeline.set_byte_position(new_pos);
        Ok(new_pos)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, io, sync::Arc};

    use kithara_storage::WaitOutcome;

    use super::*;
    use crate::{NullStreamContext, ReadOutcome, Source, StreamContext};

    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    struct ScriptSource {
        data: Vec<u8>,
        reads: VecDeque<ReadOutcome>,
        timeline: Timeline,
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
                data,
                reads: reads.into_iter().collect(),
                timeline,
                waits: waits.into_iter().collect(),
            }
        }
    }

    impl Source for ScriptSource {
        type Error = io::Error;

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

        fn len(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }

        fn timeline(&self) -> Timeline {
            self.timeline.clone()
        }
    }

    struct DummyType;

    impl StreamType for DummyType {
        type Config = ();
        type Error = io::Error;
        type Events = ();
        type Source = ScriptSource;

        async fn create(_config: Self::Config) -> Result<Self::Source, Self::Error> {
            Err(io::Error::other("not used in unit tests"))
        }

        fn build_stream_context(
            _source: &Self::Source,
            timeline: Timeline,
        ) -> Arc<dyn StreamContext> {
            Arc::new(NullStreamContext::new(timeline))
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
        let mut stream = Stream::<DummyType> { source, timeline };
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
        let mut stream = Stream::<DummyType> { source, timeline };
        let mut buf = [0u8; 4];

        let err = stream
            .read(&mut buf)
            .expect_err("flushing read must return error");
        // Uses `Other` (not `Interrupted`) so that Symphonia propagates
        // the error instead of silently retrying.
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }
}
