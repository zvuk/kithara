//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait
//! and `FileDownloader` implementing `Downloader` trait.

use std::sync::Arc;

use futures::StreamExt;
use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_storage::{OpenMode, ResourceExt, ResourceStatus, StorageOptions, StorageResource};
use kithara_stream::{Backend, Downloader, StreamType, Writer, WriterItem};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{FileConfig, FileSrc},
    error::SourceError,
    events::FileEvent,
    session::{FileResource, FileSource, FileStreamState, Progress},
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
        let asset_root = asset_root_for_url(&url);

        let store = AssetStoreBuilder::new()
            .root_dir(&config.store.cache_dir)
            .asset_root(&asset_root)
            .evict_config(config.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        let net_client = HttpClient::new(config.net.clone());

        let state = FileStreamState::create(
            Arc::new(store),
            net_client.clone(),
            url,
            cancel.clone(),
            config.events_tx.clone(),
            config.events_channel_capacity,
        )
        .await?;

        let progress = Arc::new(Progress::new());

        // Skip download if resource is already cached (committed).
        if matches!(state.res().status(), ResourceStatus::Committed { .. }) {
            tracing::debug!("file already cached, skipping download");
            let total = state.len().unwrap_or(0);
            progress.set_download_pos(total);
            let _ = state
                .events()
                .send(FileEvent::DownloadComplete { total_bytes: total });
        } else {
            // Create downloader
            let downloader = FileDownloader::new(
                &net_client,
                state.clone(),
                progress.clone(),
                state.events().clone(),
                config.look_ahead_bytes,
            )
            .await;

            // Spawn downloader as background task.
            // Backend is leaked intentionally — downloader runs until cancel or completion.
            // The JoinHandle is detached: the tokio task continues running independently.
            std::mem::forget(Backend::new(downloader, cancel));
        }

        // Create source
        let source = FileSource::new(
            FileResource::Asset(state.res().clone()),
            progress,
            state.events().clone(),
            state.len(),
        );

        Ok(source)
    }
}

/// Background file downloader implementing `Downloader`.
///
/// Wraps a `Writer`, converting chunk events to `FileEvent`.
pub struct FileDownloader {
    writer: Writer,
    progress: Arc<Progress>,
    events_tx: broadcast::Sender<FileEvent>,
    total: Option<u64>,
    look_ahead_bytes: u64,
}

impl FileDownloader {
    async fn new(
        net_client: &HttpClient,
        state: Arc<FileStreamState>,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
        look_ahead_bytes: u64,
    ) -> Self {
        let url = state.url().clone();
        let total = state.len();
        let res = state.res().clone();
        let cancel = state.cancel().clone();

        let writer = match net_client.stream(url, None).await {
            Ok(stream) => Writer::new(stream, res, cancel),
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
            progress,
            events_tx,
            total,
            look_ahead_bytes,
        }
    }
}

impl Downloader for FileDownloader {
    async fn step(&mut self) -> bool {
        // Backpressure: wait if too far ahead of reader.
        loop {
            let advanced = self.progress.notified_reader_advance();
            tokio::pin!(advanced);

            let download_pos = self.progress.download_pos();
            let reader_pos = self.progress.read_pos();

            if download_pos.saturating_sub(reader_pos) <= self.look_ahead_bytes {
                break;
            }

            advanced.await;
        }

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
            Ok(WriterItem::Completed { total_bytes }) => {
                let _ = self
                    .events_tx
                    .send(FileEvent::DownloadComplete { total_bytes });
                false
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
