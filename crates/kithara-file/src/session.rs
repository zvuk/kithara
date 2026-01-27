#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use kithara_assets::{
    AssetStore, Assets, CachedAssets, DiskAssetStore, EvictAssets, LeaseGuard, LeaseResource,
    ProcessedResource, ProcessingAssets,
};
use kithara_net::{HttpClient, Net};
use kithara_storage::{StreamingResource, StreamingResourceExt};
use kithara_stream::{Source, StreamError, StreamResult, WaitOutcome};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::trace;
use url::Url;

use crate::{error::SourceError, events::FileEvent};

pub(crate) type AssetResourceType = LeaseResource<
    ProcessedResource<StreamingResource, ()>,
    LeaseGuard<CachedAssets<ProcessingAssets<EvictAssets<DiskAssetStore>, ()>>>,
>;

#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) url: Url,
    pub(crate) cancel: CancellationToken,
    pub(crate) res: AssetResourceType,
    pub(crate) events: broadcast::Sender<FileEvent>,
    pub(crate) len: Option<u64>,
}

impl FileStreamState {
    pub(crate) async fn create(
        assets: Arc<AssetStore>,
        net_client: HttpClient,
        url: Url,
        cancel: CancellationToken,
        events_tx: Option<broadcast::Sender<FileEvent>>,
        events_channel_capacity: usize,
    ) -> Result<Arc<Self>, SourceError> {
        let headers = net_client.head(url.clone(), None).await?;
        let len = headers
            .get("content-length")
            .or_else(|| headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok());

        let key = (&url).into();
        let res = assets
            .open_streaming_resource(&key)
            .await
            .map_err(SourceError::Assets)?;

        // Use provided events_tx or create one with configured capacity
        let events =
            events_tx.unwrap_or_else(|| broadcast::channel(events_channel_capacity.max(1)).0);

        Ok(Arc::new(FileStreamState {
            url,
            cancel,
            res,
            events,
            len,
        }))
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn res(&self) -> &AssetResourceType {
        &self.res
    }

    pub(crate) fn events(&self) -> &broadcast::Sender<FileEvent> {
        &self.events
    }

    pub(crate) fn len(&self) -> Option<u64> {
        self.len
    }

    pub(crate) fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
}

/// Progress tracker for download and playback positions.
#[derive(Debug)]
pub struct Progress {
    read_pos: std::sync::atomic::AtomicU64,
    download_pos: std::sync::atomic::AtomicU64,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            read_pos: std::sync::atomic::AtomicU64::new(0),
            download_pos: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn set_read_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.read_pos.store(v, Ordering::Relaxed);
    }

    pub fn set_download_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.download_pos.store(v, Ordering::Relaxed);
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SessionSource {
    res: AssetResourceType,
    progress: Arc<Progress>,
    events: broadcast::Sender<FileEvent>,
    len: Option<u64>,
}

impl SessionSource {
    pub(crate) fn new(
        res: AssetResourceType,
        progress: Arc<Progress>,
        events: broadcast::Sender<FileEvent>,
        len: Option<u64>,
    ) -> Self {
        Self {
            res,
            progress,
            events,
            len,
        }
    }

    pub fn events(&self) -> broadcast::Receiver<FileEvent> {
        self.events.subscribe()
    }
}

#[async_trait]
impl Source for SessionSource {
    type Item = u8;
    type Error = SourceError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        trace!(
            start = range.start,
            end = range.end,
            "kithara-file SessionSource wait_range begin"
        );

        match self
            .res
            .wait_range(range)
            .await
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))?
        {
            kithara_storage::WaitOutcome::Ready => {
                trace!("kithara-file SessionSource wait_range -> Ready");
                Ok(WaitOutcome::Ready)
            }
            kithara_storage::WaitOutcome::Eof => {
                trace!("kithara-file SessionSource wait_range -> Eof");
                Ok(WaitOutcome::Eof)
            }
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        trace!(
            offset,
            len = buf.len(),
            "kithara-file SessionSource read_at begin"
        );
        let bytes_read = self
            .res
            .read_at(offset, buf)
            .await
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))?;

        let new_pos = offset.saturating_add(bytes_read as u64);
        self.progress.set_read_pos(new_pos);
        let percent = self
            .len
            .map(|len| ((new_pos as f64 / len as f64) * 100.0).min(100.0) as f32);
        let _ = self.events.send(FileEvent::PlaybackProgress {
            position: new_pos,
            percent,
        });

        trace!(
            offset,
            requested = buf.len(),
            got = bytes_read,
            "kithara-file SessionSource read_at done"
        );
        Ok(bytes_read)
    }

    fn len(&self) -> Option<u64> {
        self.len
    }
}
