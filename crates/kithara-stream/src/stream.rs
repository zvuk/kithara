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

use crate::{backend::BackendAccess, reader::Reader, MediaInfo};

/// Defines a stream type and how to create it.
///
/// This trait is implemented by marker types (`Hls`, `File`) in their respective crates.
/// The implementation provides the config type and backend type.
pub trait StreamType: Send + 'static {
    /// Configuration for this stream type.
    type Config: Default + Send;

    /// Backend implementing `BackendAccess`.
    type Backend: BackendAccess;

    /// Error type for stream creation.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Create the backend from configuration.
    ///
    /// The backend will be wrapped in `Reader` by `Stream::new()`.
    fn create_backend(
        config: Self::Config,
    ) -> impl Future<Output = Result<Self::Backend, Self::Error>> + Send;
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
/// Stream holds a `Reader<T::Backend>` which provides sync Read + Seek.
pub struct Stream<T: StreamType> {
    reader: Reader<T::Backend>,
    media_info: Option<MediaInfo>,
    pending_format_change: Option<MediaInfo>,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    pub async fn new(config: T::Config) -> Result<Self, T::Error> {
        let backend = T::create_backend(config).await?;
        Ok(Self {
            reader: Reader::new(backend),
            media_info: None,
            pending_format_change: None,
        })
    }

    /// Create a stream from an existing backend.
    pub fn from_backend(backend: T::Backend) -> Self {
        Self {
            reader: Reader::new(backend),
            media_info: None,
            pending_format_change: None,
        }
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.reader.position()
    }

    /// Get current media info if known.
    ///
    /// First checks locally stored info, then delegates to backend.
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

    /// Get current segment byte range.
    pub fn current_segment_range(&self) -> std::ops::Range<u64> {
        self.reader.current_segment_range()
    }
}

impl<T: StreamType> Read for Stream<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl<T: StreamType> Seek for Stream<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.reader.seek(pos)
    }
}
