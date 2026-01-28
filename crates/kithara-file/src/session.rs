#![forbid(unsafe_code)]

use std::sync::Arc;

use bytes::Bytes;
use kanal::{Receiver, Sender};
use kithara_assets::{
    AssetStore, Assets, CachedAssets, DiskAssetStore, EvictAssets, LeaseGuard, LeaseResource,
    ProcessedResource, ProcessingAssets,
};
use kithara_net::{HttpClient, Net};
use kithara_storage::{StreamingResource, StreamingResourceExt, WaitOutcome};
use kithara_stream::{BackendCommand, BackendResponse, RandomAccessBackend};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
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

/// File backend implementing RandomAccessBackend.
///
/// Spawns an async worker that reads from storage and sends data via channel.
pub struct FileBackend {
    cmd_tx: Sender<BackendCommand>,
    data_rx: Receiver<BackendResponse>,
    len: Option<u64>,
}

impl FileBackend {
    /// Create new file backend.
    ///
    /// Spawns async worker that handles read commands.
    pub fn new(
        res: AssetResourceType,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
        len: Option<u64>,
        cancel: CancellationToken,
    ) -> Self {
        let (cmd_tx, cmd_rx) = kanal::bounded(4);
        let (data_tx, data_rx) = kanal::bounded(4);

        // Spawn async worker
        tokio::spawn(Self::run_worker(
            res, progress, events_tx, len, cancel, cmd_rx, data_tx,
        ));

        Self {
            cmd_tx,
            data_rx,
            len,
        }
    }

    async fn run_worker(
        res: AssetResourceType,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
        len: Option<u64>,
        cancel: CancellationToken,
        cmd_rx: Receiver<BackendCommand>,
        data_tx: Sender<BackendResponse>,
    ) {
        debug!("FileBackend worker started");

        loop {
            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    debug!("FileBackend worker cancelled");
                    break;
                }

                cmd = cmd_rx.as_async().recv() => {
                    let Ok(cmd) = cmd else {
                        debug!("FileBackend cmd channel closed");
                        break;
                    };

                    match cmd {
                        BackendCommand::Read { offset, len: read_len } => {
                            let response = Self::handle_read(
                                &res, &progress, &events_tx, len, offset, read_len
                            ).await;

                            if data_tx.as_async().send(response).await.is_err() {
                                debug!("FileBackend data channel closed");
                                break;
                            }
                        }
                        BackendCommand::Seek { offset } => {
                            trace!(offset, "FileBackend seek command (no-op for file)");
                            // For file backend, seek is handled by read offset
                        }
                        BackendCommand::Stop => {
                            debug!("FileBackend stop command");
                            break;
                        }
                    }
                }
            }
        }

        debug!("FileBackend worker stopped");
    }

    async fn handle_read(
        res: &AssetResourceType,
        progress: &Progress,
        events_tx: &broadcast::Sender<FileEvent>,
        total_len: Option<u64>,
        offset: u64,
        len: usize,
    ) -> BackendResponse {
        trace!(offset, len, "FileBackend read request");

        // Wait for range to be available
        let range = offset..offset.saturating_add(len as u64);
        match res.wait_range(range).await {
            Ok(WaitOutcome::Ready) => {}
            Ok(WaitOutcome::Eof) => {
                trace!("FileBackend EOF");
                return BackendResponse::Eof;
            }
            Err(e) => {
                return BackendResponse::Error(format!("wait_range error: {}", e));
            }
        }

        // Read data
        let mut buf = vec![0u8; len];
        match res.read_at(offset, &mut buf).await {
            Ok(n) => {
                if n == 0 {
                    return BackendResponse::Eof;
                }

                buf.truncate(n);

                // Update progress
                let new_pos = offset.saturating_add(n as u64);
                progress.set_read_pos(new_pos);

                // Send event
                let percent = total_len
                    .map(|total| ((new_pos as f64 / total as f64) * 100.0).min(100.0) as f32);
                let _ = events_tx.send(FileEvent::PlaybackProgress {
                    position: new_pos,
                    percent,
                });

                trace!(offset, bytes = n, "FileBackend read complete");
                BackendResponse::Data(Bytes::from(buf))
            }
            Err(e) => BackendResponse::Error(format!("read_at error: {}", e)),
        }
    }
}

impl RandomAccessBackend for FileBackend {
    fn data_rx(&self) -> &Receiver<BackendResponse> {
        &self.data_rx
    }

    fn cmd_tx(&self) -> &Sender<BackendCommand> {
        &self.cmd_tx
    }

    fn len(&self) -> Option<u64> {
        self.len
    }
}
