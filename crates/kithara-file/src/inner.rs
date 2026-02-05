//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait
//! and `FileDownloader` implementing `Downloader` trait.

use std::sync::Arc;

use futures::StreamExt;
use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::{Headers, HttpClient};
use kithara_storage::{OpenMode, ResourceExt, ResourceStatus, StorageOptions, StorageResource};
use kithara_stream::{Backend, Downloader, StreamType, Writer, WriterItem};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{FileConfig, FileSrc},
    error::SourceError,
    events::FileEvent,
    session::{
        AssetResourceType, FileResource, FileSource, FileStreamState, Progress, SharedFileState,
    },
};

/// Marker type for file streaming.
pub struct File;

impl StreamType for File {
    type Config = FileConfig;
    type Source = FileSource;
    type Error = SourceError;
    type Event = FileEvent;

    fn ensure_events(config: &mut Self::Config) -> broadcast::Receiver<Self::Event> {
        if config.events_tx.is_none() {
            let capacity = config.events_channel_capacity.max(1);
            config.events_tx = Some(broadcast::channel(capacity).0);
        }
        match config.events_tx {
            Some(ref tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        }
    }

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        let cancel = config.cancel.clone().unwrap_or_default();
        let src = config.src.clone();

        match src {
            FileSrc::Local(path) => Self::create_local(path, config, cancel),
            FileSrc::Remote(url) => Self::create_remote(url, config, cancel).await,
        }
    }
}

impl File {
    /// Create a source for a local file.
    ///
    /// Opens the file directly via `StorageResource` in `ReadOnly` mode,
    /// skipping network, AssetStore, and background downloader entirely.
    fn create_local(
        path: std::path::PathBuf,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, SourceError> {
        if !path.exists() {
            return Err(SourceError::InvalidPath(format!(
                "file not found: {}",
                path.display()
            )));
        }

        let opts = StorageOptions {
            path,
            initial_len: None,
            mode: OpenMode::ReadOnly,
            cancel,
        };
        let resource = StorageResource::open(opts)?;
        let len = resource.len();

        let events = config
            .events_tx
            .unwrap_or_else(|| broadcast::channel(config.events_channel_capacity.max(1)).0);

        let progress = Arc::new(Progress::new());
        // Local file is fully available — mark download as complete.
        let total = len.unwrap_or(0);
        progress.set_download_pos(total);
        let _ = events.send(FileEvent::DownloadComplete { total_bytes: total });

        let source = FileSource::new(FileResource::Local(resource), progress, events, len);

        Ok(source)
    }

    /// Create a source for a remote file (HTTP/HTTPS).
    async fn create_remote(
        url: url::Url,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, SourceError> {
        let asset_root = asset_root_for_url(&url, config.name.as_deref());

        let store = AssetStoreBuilder::new()
            .root_dir(&config.store.cache_dir)
            .asset_root(&asset_root)
            .evict_config(config.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        let net_client = HttpClient::new(config.net.clone());

        let state = FileStreamState::create(
            Arc::new(store),
            &net_client,
            url,
            cancel.clone(),
            config.events_tx.clone(),
            config.events_channel_capacity,
        )
        .await?;

        let progress = Arc::new(Progress::new());
        let shared = Arc::new(SharedFileState::new());

        // Determine if the resource is a complete cache or needs downloading.
        let is_partial = match state.res().status() {
            ResourceStatus::Committed { final_len } => {
                // File on disk might be smaller than HEAD Content-Length → partial.
                state
                    .len()
                    .zip(final_len)
                    .is_some_and(|(expected, actual)| actual < expected)
            }
            _ => false,
        };

        if matches!(state.res().status(), ResourceStatus::Committed { .. }) && !is_partial {
            // Fully cached — no download needed.
            tracing::debug!("file already cached, skipping download");
            let total = state.len().unwrap_or(0);
            progress.set_download_pos(total);
            let _ = state
                .events()
                .send(FileEvent::DownloadComplete { total_bytes: total });
        } else {
            if is_partial {
                // Partial cache: reactivate resource for continued writing.
                tracing::debug!("partial cache detected, reactivating for on-demand download");
                state.res().reactivate().map_err(SourceError::Storage)?;
            }

            // Create downloader with shared state for on-demand loading.
            // For partial cache, downloader starts in on-demand-only mode
            // (sequential stream from scratch, but partial data already on disk).
            let downloader = FileDownloader::new(
                &net_client,
                state.clone(),
                progress.clone(),
                state.events().clone(),
                config.look_ahead_bytes,
                shared.clone(),
            )
            .await;

            // Spawn downloader as background task.
            // Backend is leaked intentionally — downloader runs until cancel or completion.
            // The JoinHandle is detached: the tokio task continues running independently.
            std::mem::forget(Backend::new(downloader, cancel));
        }

        // Create source with shared state for on-demand loading
        let source = FileSource::with_shared(
            FileResource::Asset(state.res().clone()),
            progress,
            state.events().clone(),
            state.len(),
            shared,
        );

        Ok(source)
    }
}

/// Background file downloader implementing `Downloader`.
///
/// Wraps a `Writer`, converting chunk events to `FileEvent`.
/// Supports two modes:
/// - Sequential: initial download from start
/// - On-demand: HTTP Range requests when reader seeks beyond downloaded data
pub struct FileDownloader {
    writer: Writer,
    res: AssetResourceType,
    progress: Arc<Progress>,
    events_tx: broadcast::Sender<FileEvent>,
    total: Option<u64>,
    look_ahead_bytes: u64,
    shared: Arc<SharedFileState>,
    net_client: HttpClient,
    url: url::Url,
    cancel: CancellationToken,
    /// True if sequential stream has ended (partial or complete).
    sequential_ended: bool,
}

impl FileDownloader {
    async fn new(
        net_client: &HttpClient,
        state: Arc<FileStreamState>,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
        look_ahead_bytes: u64,
        shared: Arc<SharedFileState>,
    ) -> Self {
        let url = state.url().clone();
        let total = state.len();
        let res = state.res().clone();
        let cancel = state.cancel().clone();

        let writer = match net_client.stream(url.clone(), None).await {
            Ok(stream) => Writer::new(stream, res.clone(), cancel.clone()),
            Err(e) => {
                tracing::warn!("failed to open stream: {}", e);
                res.fail(e.to_string());
                let _ = events_tx.send(FileEvent::DownloadError {
                    error: e.to_string(),
                });
                // Return a writer from an empty stream so step() returns false immediately
                let empty = futures::stream::empty();
                let boxed: kithara_net::ByteStream = Box::pin(empty);
                Writer::new(boxed, state.res().clone(), CancellationToken::new())
            }
        };

        Self {
            writer,
            res,
            progress,
            events_tx,
            total,
            look_ahead_bytes,
            shared,
            net_client: net_client.clone(),
            url,
            cancel,
            sequential_ended: false,
        }
    }

    /// Fetch a range on-demand via HTTP Range request.
    async fn fetch_range(&mut self, range: std::ops::Range<u64>) {
        let range_header = format!("bytes={}-{}", range.start, range.end.saturating_sub(1));
        let mut headers = Headers::new();
        headers.insert("Range", range_header);

        match self
            .net_client
            .stream(self.url.clone(), Some(headers))
            .await
        {
            Ok(stream) => {
                let mut on_demand_writer =
                    Writer::with_offset(stream, self.res.clone(), self.cancel.clone(), range.start);

                while let Some(result) = on_demand_writer.next().await {
                    match result {
                        Ok(WriterItem::ChunkWritten { .. }) => {}
                        Ok(WriterItem::StreamEnded { .. }) => break,
                        Err(e) => {
                            tracing::warn!(?e, "on-demand range download failed");
                            break;
                        }
                    }
                }
                tracing::debug!(
                    start = range.start,
                    end = range.end,
                    "on-demand range fetched"
                );
            }
            Err(e) => {
                tracing::warn!(?e, "failed to start on-demand range request");
            }
        }
        // No explicit notification needed: Writer::write_at() notifies
        // the resource's internal condvar, which unblocks FileSource::wait_range().
    }
}

impl Downloader for FileDownloader {
    async fn step(&mut self) -> bool {
        // Check for on-demand range requests first (higher priority)
        if let Some(range) = self.shared.pop_range_request() {
            tracing::debug!(
                start = range.start,
                end = range.end,
                "processing on-demand range request"
            );
            self.fetch_range(range).await;
            return true;
        }

        // If sequential stream ended, wait for on-demand requests or cancel
        if self.sequential_ended {
            tokio::select! {
                _ = self.shared.reader_needs_data.notified() => {
                    return true; // Loop will check pop_range_request
                }
                _ = self.cancel.cancelled() => {
                    return false;
                }
            }
        }

        // Backpressure: wait if too far ahead of reader.
        loop {
            let advanced = self.progress.notified_reader_advance();
            tokio::pin!(advanced);

            let download_pos = self.progress.download_pos();
            let reader_pos = self.progress.read_pos();

            if download_pos.saturating_sub(reader_pos) <= self.look_ahead_bytes {
                break;
            }

            // Also break on on-demand requests while waiting
            tokio::select! {
                _ = advanced => {}
                _ = self.shared.reader_needs_data.notified() => {
                    break;
                }
            }
        }

        // Sequential download continues
        let Some(result) = self.writer.next().await else {
            return false;
        };

        match result {
            Ok(WriterItem::ChunkWritten {
                offset,
                len: chunk_len,
            }) => {
                let download_offset = offset + chunk_len as u64;
                self.progress.set_download_pos(download_offset);
                let _ = self.events_tx.send(FileEvent::DownloadProgress {
                    offset: download_offset,
                    total: self.total,
                });
                true
            }
            Ok(WriterItem::StreamEnded { total_bytes }) => {
                // Check if download is complete
                let is_complete = self
                    .total
                    .map(|expected| total_bytes >= expected)
                    .unwrap_or(true); // No Content-Length → assume complete

                self.sequential_ended = true;

                if is_complete {
                    // Complete download — commit resource explicitly
                    if let Err(e) = self.res.commit(Some(total_bytes)) {
                        tracing::error!(?e, "failed to commit resource");
                        self.res.fail(format!("commit failed: {}", e));
                        let _ = self.events_tx.send(FileEvent::DownloadError {
                            error: format!("commit failed: {}", e),
                        });
                        return false;
                    }
                    let _ = self
                        .events_tx
                        .send(FileEvent::DownloadComplete { total_bytes });
                    false
                } else {
                    // Partial download — do NOT commit, leave resource Active.
                    // Continue running to handle on-demand Range requests.
                    tracing::warn!(
                        total_bytes,
                        expected = ?self.total,
                        "stream ended early — partial download, switching to on-demand mode"
                    );
                    let _ = self.events_tx.send(FileEvent::DownloadProgress {
                        offset: total_bytes,
                        total: self.total,
                    });
                    true // Continue for on-demand requests
                }
            }
            Err(e) => {
                tracing::warn!("download failed: {}", e);
                let _ = self.events_tx.send(FileEvent::DownloadError {
                    error: e.to_string(),
                });
                false
            }
        }
    }
}
