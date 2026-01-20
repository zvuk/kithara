//! `StreamSource` wrapper providing `Source` + events access.

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use kithara_storage::WaitOutcome;
use tokio::sync::broadcast;
use url::Url;

use crate::{
    MediaInfo, Source, StreamError, StreamResult, SyncReader, SyncReaderParams,
    facade::SourceFactory,
};

/// Unified stream source wrapper providing `Source` trait and event access.
///
/// Generic over `S: SourceFactory` to support both File and HLS sources.
///
/// ## Usage
///
/// ```ignore
/// use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
/// use kithara_hls::{Hls, HlsParams};
///
/// // Async source with events
/// let source = StreamSource::<Hls>::open(url, HlsParams::default()).await?;
/// let events = source.events();  // broadcast::Receiver<HlsEvent>
///
/// // Sync reader with events (for decoders)
/// let reader = SyncReader::<StreamSource<Hls>>::open(
///     url,
///     HlsParams::default(),
///     SyncReaderParams::default()
/// ).await?;
/// let events = reader.events();
/// ```
pub struct StreamSource<S: SourceFactory> {
    inner: Arc<S::SourceImpl>,
    pub(crate) events_tx: broadcast::Sender<S::Event>,
}

impl<S: SourceFactory> StreamSource<S> {
    /// Open a stream source from URL with the given parameters.
    ///
    /// Must be called from within a Tokio runtime context.
    pub async fn open(
        url: Url,
        params: S::Params,
    ) -> Result<Self, StreamError<<S::SourceImpl as Source>::Error>> {
        let opened = S::open(url, params).await?;
        Ok(Self {
            inner: opened.source,
            events_tx: opened.events_tx,
        })
    }

    /// Subscribe to stream events.
    ///
    /// Returns a receiver for source-specific events (e.g., `FileEvent`, `HlsEvent`).
    pub fn events(&self) -> broadcast::Receiver<S::Event> {
        self.events_tx.subscribe()
    }

    /// Get reference to the inner source.
    pub fn inner(&self) -> &Arc<S::SourceImpl> {
        &self.inner
    }
}

#[async_trait]
impl<S: SourceFactory> Source for StreamSource<S> {
    type Item = u8;
    type Error = <S::SourceImpl as Source>::Error;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        self.inner.wait_range(range).await
    }

    async fn read_at(
        &self,
        offset: u64,
        buf: &mut [Self::Item],
    ) -> StreamResult<usize, Self::Error> {
        self.inner.read_at(offset, buf).await
    }

    fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.inner.media_info()
    }
}

// Specialized impl for SyncReader<StreamSource<S>>
impl<S: SourceFactory> SyncReader<StreamSource<S>> {
    /// Open a sync reader from URL with the given parameters.
    ///
    /// Convenience method that creates `StreamSource` and wraps it in `SyncReader`.
    /// Must be called from within a Tokio runtime context.
    pub async fn open(
        url: Url,
        params: S::Params,
        reader_params: SyncReaderParams,
    ) -> Result<Self, StreamError<<S::SourceImpl as Source>::Error>> {
        let source = StreamSource::<S>::open(url, params).await?;
        Ok(Self::new(Arc::new(source), reader_params))
    }

    /// Subscribe to stream events.
    ///
    /// Returns a receiver for source-specific events (e.g., `FileEvent`, `HlsEvent`).
    pub fn events(&self) -> broadcast::Receiver<S::Event> {
        self.source_ref().events_tx.subscribe()
    }
}
