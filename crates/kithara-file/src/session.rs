#![forbid(unsafe_code)]

use std::{path::Path, sync::Arc};

use kithara_assets::{
    AssetStore, Assets, DiskAssetStore, EvictAssets, LeaseGuard, LeaseResource,
    ProcessedResource, ProcessingAssets,
};
use kithara_net::{HttpClient, Net};
use kithara_storage::{ResourceExt, ResourceStatus, StorageResource, StorageResult, WaitOutcome};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::trace;
use url::Url;

use crate::{error::SourceError, events::FileEvent};

pub(crate) type AssetResourceType = LeaseResource<
    ProcessedResource<StorageResource, ()>,
    LeaseGuard<ProcessingAssets<EvictAssets<DiskAssetStore>, ()>>,
>;

/// Unified file resource: either a remote asset (via AssetStore) or a local file.
#[derive(Clone, Debug)]
pub(crate) enum FileResource {
    /// Remote file — managed by AssetStore (caching, eviction, leases).
    Asset(AssetResourceType),
    /// Local file — direct StorageResource in ReadOnly mode.
    Local(StorageResource),
}

impl ResourceExt for FileResource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        match self {
            Self::Asset(r) => r.read_at(offset, buf),
            Self::Local(r) => r.read_at(offset, buf),
        }
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        match self {
            Self::Asset(r) => r.write_at(offset, data),
            Self::Local(r) => r.write_at(offset, data),
        }
    }

    fn wait_range(&self, range: std::ops::Range<u64>) -> StorageResult<WaitOutcome> {
        match self {
            Self::Asset(r) => r.wait_range(range),
            Self::Local(r) => r.wait_range(range),
        }
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        match self {
            Self::Asset(r) => r.commit(final_len),
            Self::Local(r) => r.commit(final_len),
        }
    }

    fn fail(&self, reason: String) {
        match self {
            Self::Asset(r) => r.fail(reason),
            Self::Local(r) => r.fail(reason),
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Asset(r) => r.path(),
            Self::Local(r) => r.path(),
        }
    }

    fn len(&self) -> Option<u64> {
        match self {
            Self::Asset(r) => r.len(),
            Self::Local(r) => r.len(),
        }
    }

    fn status(&self) -> ResourceStatus {
        match self {
            Self::Asset(r) => r.status(),
            Self::Local(r) => r.status(),
        }
    }

    fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        match self {
            Self::Asset(r) => r.read_into(buf),
            Self::Local(r) => r.read_into(buf),
        }
    }

    fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        match self {
            Self::Asset(r) => r.write_all(data),
            Self::Local(r) => r.write_all(data),
        }
    }
}

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
        let res = assets.open_resource(&key).map_err(SourceError::Assets)?;

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
pub struct Progress {
    read_pos: std::sync::atomic::AtomicU64,
    download_pos: std::sync::atomic::AtomicU64,
    /// Source -> Downloader: reader advanced, may resume downloading.
    reader_advanced: Notify,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            read_pos: std::sync::atomic::AtomicU64::new(0),
            download_pos: std::sync::atomic::AtomicU64::new(0),
            reader_advanced: Notify::new(),
        }
    }

    pub fn read_pos(&self) -> u64 {
        use std::sync::atomic::Ordering;
        self.read_pos.load(Ordering::Acquire)
    }

    pub fn download_pos(&self) -> u64 {
        use std::sync::atomic::Ordering;
        self.download_pos.load(Ordering::Acquire)
    }

    pub fn set_read_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.read_pos.store(v, Ordering::Release);
        self.reader_advanced.notify_one();
    }

    pub fn set_download_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.download_pos.store(v, Ordering::Release);
    }

    /// Register for reader advance notification.
    /// Must be called BEFORE checking positions to avoid race.
    pub fn notified_reader_advance(&self) -> tokio::sync::futures::Notified<'_> {
        self.reader_advanced.notified()
    }

    /// Signal that the reader needs data (wake paused downloader).
    pub fn signal_reader_advanced(&self) {
        self.reader_advanced.notify_one();
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

/// File source implementing Source trait (sync).
///
/// Wraps storage resource with progress tracking and event emission.
pub struct FileSource {
    res: FileResource,
    progress: Arc<Progress>,
    events_tx: broadcast::Sender<FileEvent>,
    len: Option<u64>,
}

impl FileSource {
    /// Create new file source.
    pub(crate) fn new(
        res: FileResource,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
        len: Option<u64>,
    ) -> Self {
        Self {
            res,
            progress,
            events_tx,
            len,
        }
    }
}

impl kithara_stream::Source for FileSource {
    type Item = u8;
    type Error = SourceError;

    fn wait_range(
        &mut self,
        range: std::ops::Range<u64>,
    ) -> kithara_stream::StreamResult<WaitOutcome, SourceError> {
        use kithara_stream::StreamError;

        // Update read position so downloader knows where reader needs data.
        // This prevents backpressure deadlock when symphonia seeks ahead
        // (e.g., SeekFrom::End) while downloader is still near the beginning.
        if range.start > self.progress.read_pos() {
            self.progress.set_read_pos(range.start);
        }

        self.res
            .wait_range(range)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))
    }

    fn read_at(
        &mut self,
        offset: u64,
        buf: &mut [u8],
    ) -> kithara_stream::StreamResult<usize, SourceError> {
        use kithara_stream::StreamError;

        let n = self
            .res
            .read_at(offset, buf)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))?;

        if n > 0 {
            // Update progress
            let new_pos = offset.saturating_add(n as u64);
            self.progress.set_read_pos(new_pos);

            let _ = self.events_tx.send(FileEvent::PlaybackProgress {
                position: new_pos,
                total: self.len,
            });

            trace!(offset, bytes = n, "FileSource read complete");
        }

        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        self.len
    }
}
