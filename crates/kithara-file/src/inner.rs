//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait.

use std::sync::Arc;

use futures::StreamExt;
use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_storage::Resource;
use kithara_stream::{StreamType, Writer, WriterItem};
use tokio::sync::broadcast;

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

        // Spawn download writer
        spawn_download_writer(
            &net_client,
            state.clone(),
            progress.clone(),
            state.events().clone(),
        );

        // Create source and backend
        let source = FileSource::new(
            state.res().clone(),
            progress,
            state.events().clone(),
            state.len(),
        );
        let backend = kithara_stream::Backend::new(source);

        Ok(backend)
    }
}

fn spawn_download_writer(
    net_client: &HttpClient,
    state: Arc<FileStreamState>,
    progress: Arc<Progress>,
    events_tx: broadcast::Sender<FileEvent>,
) {
    let net = net_client.clone();
    let url = state.url().clone();
    let len = state.len();
    let res = state.res().clone();
    let cancel = state.cancel().clone();

    tokio::spawn(async move {
        // Open network stream first
        let stream = match net.stream(url, None).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to open stream: {}", e);
                let _ = res.fail(e.to_string()).await;
                return;
            }
        };

        let mut writer = Writer::new(stream, res, cancel);

        while let Some(result) = writer.next().await {
            match result {
                Ok(WriterItem::ChunkWritten { offset, len: chunk_len }) => {
                    let download_offset = offset + chunk_len as u64;
                    progress.set_download_pos(download_offset);
                    let percent = len.map(|total| {
                        ((download_offset as f64 / total as f64) * 100.0).min(100.0) as f32
                    });
                    let _ = events_tx.send(FileEvent::DownloadProgress {
                        offset: download_offset,
                        percent,
                    });
                }
                Ok(WriterItem::Completed { .. }) => {
                    break;
                }
                Err(e) => {
                    tracing::warn!("download failed: {}", e);
                    break;
                }
            }
        }
    });
}
