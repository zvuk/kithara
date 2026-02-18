//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait.

use std::sync::{Arc, atomic::AtomicU64};

use kithara_assets::{AssetStoreBuilder, Assets, CoverageIndex, asset_root_for_url};
use kithara_events::{EventBus, FileEvent};
use kithara_net::HttpClient;
use kithara_platform::ThreadPool;
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{Backend, NullStreamContext, StreamContext, StreamType};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{FileConfig, FileSrc},
    downloader::FileDownloader,
    error::SourceError,
    session::{FileSource, FileStreamState, Progress, SharedFileState},
};

/// Marker type for file streaming.
pub struct File;

impl StreamType for File {
    type Config = FileConfig;
    type Source = FileSource;
    type Error = SourceError;
    type Events = EventBus;

    fn thread_pool(config: &Self::Config) -> ThreadPool {
        config.thread_pool.clone()
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        let cancel = config.cancel.clone().unwrap_or_default();
        let src = config.src.clone();

        match src {
            FileSrc::Local(path) => Self::create_local(path, config, cancel),
            FileSrc::Remote(url) => Self::create_remote(url, config, cancel).await,
        }
    }

    fn build_stream_context(
        _source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(position))
    }
}

impl File {
    /// Create a source for a local file.
    ///
    /// Opens the file via `AssetStore` with an absolute `ResourceKey`,
    /// skipping network and background downloader entirely.
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

        let store = AssetStoreBuilder::new()
            .asset_root(None)
            .cache_enabled(false)
            .lease_enabled(false)
            .evict_enabled(false)
            .cancel(cancel)
            .build();

        let key = kithara_assets::ResourceKey::absolute(path);
        let res = store.open_resource(&key).map_err(SourceError::Assets)?;
        let len = res.len();

        let bus = config
            .bus
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let progress = Arc::new(Progress::new());
        // Local file is fully available — mark download as complete.
        let total = len.unwrap_or(0);
        progress.set_download_pos(total);
        bus.publish(FileEvent::DownloadComplete { total_bytes: total });

        Ok(FileSource::new(res, progress, bus, len))
    }

    /// Create a source for a remote file (HTTP/HTTPS).
    async fn create_remote(
        url: url::Url,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, SourceError> {
        let asset_root = asset_root_for_url(&url, config.name.as_deref());

        let backend = AssetStoreBuilder::new()
            .root_dir(&config.store.cache_dir)
            .asset_root(Some(asset_root.as_str()))
            .evict_config(config.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        // Open coverage index for crash-safe download tracking.
        // Only available for disk-backed storage (ephemeral/mem doesn't need it).
        let coverage_index = match &backend {
            kithara_assets::AssetsBackend::Disk(store) => store
                .open_coverage_index_resource()
                .ok()
                .map(|res| Arc::new(CoverageIndex::new(res))),
            kithara_assets::AssetsBackend::Mem(_) => None,
        };

        let net_client = HttpClient::new(config.net.clone());

        let state = FileStreamState::create(
            Arc::new(backend),
            &net_client,
            url,
            cancel.clone(),
            config.bus.clone(),
            config.event_channel_capacity,
            config.headers.clone(),
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
            state
                .bus()
                .publish(FileEvent::DownloadComplete { total_bytes: total });
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
                state.bus().clone(),
                config.look_ahead_bytes,
                shared.clone(),
                coverage_index,
            )
            .await;

            // Spawn downloader on the thread pool.
            // Backend is stored in FileSource — dropping the source cancels the downloader.
            let backend = Backend::new(downloader, &cancel, &config.thread_pool);

            // Create source with shared state and backend for on-demand loading.
            let source = FileSource::with_shared(
                state.res().clone(),
                progress,
                state.bus().clone(),
                state.len(),
                shared,
                backend,
            );

            return Ok(source);
        }

        // Fully cached — create source without backend (no downloader needed).
        let source = FileSource::new(
            state.res().clone(),
            progress,
            state.bus().clone(),
            state.len(),
        );

        Ok(source)
    }
}
