//! Unified Stream API for kithara.
//!
//! Provides a generic `Stream<S>` wrapper that implements `Read + Seek` for use with
//! audio decoders (rodio, symphonia) and exposes typed events.
//!
//! ## Usage
//!
//! ```ignore
//! use kithara_stream::Stream;
//! use kithara_file::{File, FileParams};
//!
//! let stream = Stream::<File>::open(url, params).await?;
//! let events = stream.events();  // broadcast::Receiver<FileEvent>
//! let decoder = rodio::Decoder::new(stream)?;  // stream implements Read + Seek
//! ```

use std::{
    future::Future,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use tokio::sync::broadcast;
use url::Url;

use crate::{Source, StreamError, SyncReader, SyncReaderParams};

/// Result of opening a stream source.
pub struct OpenedSource<S, E>
where
    S: Source,
{
    /// The opened source implementing random-access byte reading.
    pub source: Arc<S>,
    /// Broadcast sender for source-specific events.
    pub events_tx: broadcast::Sender<E>,
}

/// Trait for stream source factories.
///
/// Implementations create sources from URLs and parameters.
/// The associated types define the parameter, event, and source types.
///
/// ## Implementing
///
/// Higher-level crates (kithara-file, kithara-hls) implement this trait
/// for their marker types (`File`, `Hls`).
pub trait StreamSource: Send + Sync + 'static {
    /// Configuration parameters for opening the source.
    type Params: Send;

    /// Event type emitted by this source.
    type Event: Clone + Send + 'static;

    /// Concrete source type implementing random-access reads.
    type SourceImpl: Source + Send + Sync;

    /// Open a stream from the given URL with the provided parameters.
    ///
    /// Returns an `OpenedSource` containing the source and event channel.
    #[expect(clippy::type_complexity)]
    fn open(
        url: Url,
        params: Self::Params,
    ) -> impl Future<
        Output = Result<
            OpenedSource<Self::SourceImpl, Self::Event>,
            StreamError<<Self::SourceImpl as Source>::Error>,
        >,
    > + Send;
}

/// Unified stream wrapper providing `Read + Seek` and event access.
///
/// Generic over `S: StreamSource` to support both File and HLS sources.
/// Must be created from within a Tokio runtime context.
pub struct Stream<S: StreamSource> {
    reader: SyncReader<S::SourceImpl>,
    events_tx: broadcast::Sender<S::Event>,
}

impl<S: StreamSource> Stream<S> {
    /// Open a stream from URL with the given parameters.
    ///
    /// This is the primary entry point for stream creation.
    /// Must be called from within a Tokio runtime context.
    pub async fn open(
        url: Url,
        params: S::Params,
    ) -> Result<Self, StreamError<<S::SourceImpl as Source>::Error>> {
        let opened = S::open(url, params).await?;
        let reader = SyncReader::new(opened.source, SyncReaderParams::default());
        Ok(Self {
            reader,
            events_tx: opened.events_tx,
        })
    }

    /// Subscribe to stream events.
    ///
    /// Returns a receiver for source-specific events (e.g., `FileEvent`, `HlsEvent`).
    pub fn events(&self) -> broadcast::Receiver<S::Event> {
        self.events_tx.subscribe()
    }

    /// Get current read position in the stream.
    pub fn position(&self) -> u64 {
        self.reader.position()
    }
}

impl<S: StreamSource> Read for Stream<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl<S: StreamSource> Seek for Stream<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.reader.seek(pos)
    }
}
