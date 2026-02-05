//! Audio stream types and traits.
//!
//! Provides `Stream<T>` - a generic audio stream parameterized by stream type.
//!
//! Marker types (`Hls`, `File`) are defined in their respective crates
//! and implement `StreamType` trait.

#![forbid(unsafe_code)]

use std::{
    future::Future,
    io::{Read, Seek, SeekFrom},
};

use tokio::sync::broadcast;

use crate::{MediaInfo, reader::Reader, source::Source};

/// Defines a stream type and how to create it.
///
/// This trait is implemented by marker types (`Hls`, `File`) in their respective crates.
/// The implementation provides the config type and source type.
pub trait StreamType: Send + 'static {
    /// Configuration for this stream type.
    type Config: Default + Send;

    /// Source implementing `Source`.
    type Source: Source;

    /// Error type for stream creation.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Event type emitted by this stream.
    type Event: Clone + Send + 'static;

    /// Create the source from configuration.
    ///
    /// The source will be wrapped in `Reader` by `Stream::new()`.
    /// May also start background tasks (downloader) internally.
    fn create(
        config: Self::Config,
    ) -> impl Future<Output = Result<Self::Source, Self::Error>> + Send;

    /// Ensure an events channel exists in config and return a receiver.
    ///
    /// If the config already has `events_tx`, subscribes to it.
    /// Otherwise creates a new channel and sets it on the config.
    /// Called by `Stream::new()` before `create()`.
    fn ensure_events(config: &mut Self::Config) -> broadcast::Receiver<Self::Event>;
}

/// Configuration for stream behavior.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Read buffer size in bytes.
    pub read_buffer_size: usize,
    /// Number of segments to prefetch (for HLS).
    pub prefetch_segments: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            read_buffer_size: 32 * 1024, // 32KB
            prefetch_segments: 2,
        }
    }
}

/// Generic audio stream.
///
/// `T` is a marker type defining the stream source (`Hls`, `File`, etc.).
/// Stream holds a `Reader<T::Source>` which provides sync Read + Seek.
pub struct Stream<T: StreamType> {
    reader: Reader<T::Source>,
    media_info: Option<MediaInfo>,
    pending_format_change: Option<MediaInfo>,
    events_rx: Option<broadcast::Receiver<T::Event>>,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    pub async fn new(mut config: T::Config) -> Result<Self, T::Error> {
        let events_rx = T::ensure_events(&mut config);
        let source = T::create(config).await?;
        Ok(Self {
            reader: Reader::new(source),
            media_info: None,
            pending_format_change: None,
            events_rx: Some(events_rx),
        })
    }

    /// Create a stream from an existing source.
    pub fn from_source(source: T::Source) -> Self {
        Self {
            reader: Reader::new(source),
            media_info: None,
            pending_format_change: None,
            events_rx: None,
        }
    }

    /// Take the stream events receiver.
    ///
    /// Returns `Some` on first call, `None` on subsequent calls.
    /// Used by `Decoder` to forward stream events into the unified channel.
    pub fn take_events_rx(&mut self) -> Option<broadcast::Receiver<T::Event>> {
        self.events_rx.take()
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.reader.position()
    }

    /// Get current media info if known.
    ///
    /// First checks locally stored info, then delegates to source.
    pub fn media_info(&self) -> Option<MediaInfo> {
        if let Some(ref info) = self.media_info {
            return Some(info.clone());
        }
        self.reader.media_info()
    }

    /// Set media info.
    pub fn set_media_info(&mut self, info: MediaInfo) {
        self.media_info = Some(info);
    }

    /// Poll for format change.
    ///
    /// Returns `Some(MediaInfo)` if format has changed since last poll.
    pub fn poll_format_change(&mut self) -> Option<MediaInfo> {
        self.pending_format_change.take()
    }

    /// Signal a format change.
    pub fn signal_format_change(&mut self, info: MediaInfo) {
        self.pending_format_change = Some(info);
    }

    /// Get total length if known.
    pub fn len(&self) -> Option<u64> {
        self.reader.len()
    }

    /// Check if length is zero or unknown.
    pub fn is_empty(&self) -> bool {
        self.reader.is_empty()
    }

    /// Get current segment byte range (for segmented sources like HLS).
    pub fn current_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.reader.current_segment_range()
    }

    /// Get byte range of first segment with current format after ABR switch.
    ///
    /// For HLS: returns the first segment of the new variant which contains
    /// init data (ftyp/moov). This is where the decoder should be recreated.
    pub fn format_change_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.reader.format_change_segment_range()
    }
}

impl<T: StreamType> Read for Stream<T> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl<T: StreamType> Seek for Stream<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.reader.seek(pos)
    }
}
