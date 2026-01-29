//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait
//! and `FileDownloader` implementing `Downloader` trait.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_storage::Resource;
use kithara_stream::{Downloader, StreamType, Writer, WriterItem};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    error::SourceError,
    events::FileEvent,
    options::FileConfig,
    session::{FileSource, FileStreamState, Progress},
};

/// Marker type for file streaming.
pub struct File;

impl StreamType for File {
    type Config = FileConfig;
    type Backend = kithara_stream::Backend;
    type Error = SourceError;
    type Event = FileEvent;

    async fn create_backend(config: Self::Config) -> Result<Self::Backend, Self::Error> {
        let asset_root = asset_root_for_url(&config.url);
        let cancel = config.cancel.clone().unwrap_or_default();

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
            config.url,
            cancel.clone(),
            config.events_tx.clone(),
            config.events_channel_capacity,
        )
        .await?;

        let progress = Arc::new(Progress::new());

        // Create downloader
        let downloader = FileDownloader::new(
            &net_client,
            state.clone(),
            progress.clone(),
            state.events().clone(),
        )
        .await;

        // Create source and backend
        let source = FileSource::new(
            state.res().clone(),
            progress,
            state.events().clone(),
            state.len(),
        );
        let backend = kithara_stream::Backend::new(source, downloader, cancel);

        Ok(backend)
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
}

impl FileDownloader {
    async fn new(
        net_client: &HttpClient,
        state: Arc<FileStreamState>,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
    ) -> Self {
        let url = state.url().clone();
        let total = state.len();
        let res = state.res().clone();
        let cancel = state.cancel().clone();

        let writer = match net_client.stream(url, None).await {
            Ok(stream) => Writer::new(stream, res, cancel),
            Err(e) => {
                tracing::warn!("failed to open stream: {}", e);
                let _ = res.fail(e.to_string()).await;
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
        }
    }
}

#[async_trait]
impl Downloader for FileDownloader {
    async fn step(&mut self) -> bool {
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
