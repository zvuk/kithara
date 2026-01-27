//! Audio stream types and traits.
//!
//! Provides `Stream<T>` - a generic audio stream parameterized by stream type.
//!
//! Marker types (`Hls`, `File`) are defined in their respective crates
//! and implement `StreamType` trait.

#![forbid(unsafe_code)]

use std::{
    future::Future,
    io::{Read, Seek},
};

use crate::MediaInfo;

/// Defines a stream type and how to create it.
///
/// This trait is implemented by marker types (`Hls`, `File`) in their respective crates.
/// The implementation provides the config type and the inner stream type.
pub trait StreamType: Send + 'static {
    /// Configuration for this stream type.
    type Config: Default + Send;

    /// Inner stream implementing Read + Seek.
    type Inner: Read + Seek + Send;

    /// Error type for stream creation.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Create the inner stream from configuration.
    fn create(
        config: Self::Config,
    ) -> impl Future<Output = Result<Self::Inner, Self::Error>> + Send;
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
/// The actual implementation is provided by `T::Inner`.
pub struct Stream<T: StreamType> {
    inner: T::Inner,
    media_info: Option<MediaInfo>,
    pending_format_change: Option<MediaInfo>,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    pub async fn new(config: T::Config) -> Result<Self, T::Error> {
        let inner = T::create(config).await?;
        Ok(Self {
            inner,
            media_info: None,
            pending_format_change: None,
        })
    }

    /// Create a stream from an existing inner implementation.
    pub fn from_inner(inner: T::Inner) -> Self {
        Self {
            inner,
            media_info: None,
            pending_format_change: None,
        }
    }

    /// Get reference to inner stream.
    pub fn inner(&self) -> &T::Inner {
        &self.inner
    }

    /// Get mutable reference to inner stream.
    pub fn inner_mut(&mut self) -> &mut T::Inner {
        &mut self.inner
    }

    /// Consume stream and return inner.
    pub fn into_inner(self) -> T::Inner {
        self.inner
    }

    /// Get current media info if known.
    pub fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
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
}

impl<T: StreamType> Read for Stream<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<T: StreamType> Seek for Stream<T> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}
