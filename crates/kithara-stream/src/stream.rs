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
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_storage::{MaybeSend, MaybeSync, WaitOutcome};

use crate::{MediaInfo, StreamContext, ThreadPool, source::Source};

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
    type Error: std::error::Error + Send + Sync + 'static;

    /// Create the source from configuration.
    ///
    /// May also start background tasks (downloader) internally.
    fn create(config: Self::Config) -> impl Future<Output = Result<Self::Source, Self::Error>>;

    /// Extract the thread pool from config.
    ///
    /// Default returns the global rayon pool. Override for stream types
    /// that store a custom pool in their config.
    fn thread_pool(config: &Self::Config) -> ThreadPool {
        let _ = config;
        ThreadPool::default()
    }

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
    fn build_stream_context(
        source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn StreamContext>;
}

/// Generic audio stream with sync `Read + Seek`.
///
/// `T` is a marker type defining the stream source (`Hls`, `File`, etc.).
/// Stream holds the source directly and implements `Read + Seek` by calling
/// `Source::wait_range()` and `Source::read_at()`.
pub struct Stream<T: StreamType> {
    source: T::Source,
    pos: Arc<AtomicU64>,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    pub async fn new(config: T::Config) -> Result<Self, T::Error> {
        let source = T::create(config).await?;
        Ok(Self {
            source,
            pos: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.pos.load(Ordering::Relaxed)
    }

    /// Get handle to shared byte position (for `StreamContext`).
    pub fn position_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.pos)
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
    pub fn current_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.source.current_segment_range()
    }

    /// Get byte range of first segment with current format after ABR switch.
    ///
    /// For HLS: returns the first segment of the new variant which contains
    /// init data (ftyp/moov). This is where the decoder should be recreated.
    pub fn format_change_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.source.format_change_segment_range()
    }

    /// Clear variant fence, allowing reads from the next variant.
    pub fn clear_variant_fence(&mut self) {
        self.source.clear_variant_fence();
    }
}

impl<T: StreamType> Read for Stream<T> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let pos = self.pos.load(Ordering::Relaxed);
        let range = pos..pos.saturating_add(buf.len() as u64);

        // Wait for data to be available (blocking)
        match self
            .source
            .wait_range(range)
            .map_err(|e| std::io::Error::other(e.to_string()))?
        {
            WaitOutcome::Ready => {}
            WaitOutcome::Eof => return Ok(0),
        }

        // Read data directly from source
        let n = self
            .source
            .read_at(pos, buf)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        self.pos
            .store(pos.saturating_add(n as u64), Ordering::Relaxed);
        Ok(n)
    }
}

impl<T: StreamType> Seek for Stream<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let current = self.pos.load(Ordering::Relaxed);
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (current as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "seek from end requires known length",
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        let new_pos = new_pos as u64;

        if let Some(len) = self.source.len()
            && new_pos > len
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "seek past EOF: new_pos={new_pos} len={len} current_pos={current} seek_from={pos:?}",
                ),
            ));
        }

        self.pos.store(new_pos, Ordering::Relaxed);
        Ok(new_pos)
    }
}
