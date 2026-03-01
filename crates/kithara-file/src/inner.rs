//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait.

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_events::{EventBus, FileEvent};
use kithara_net::HttpClient;
use kithara_storage::{ResourceExt, ResourceStatus};
#[cfg(not(target_arch = "wasm32"))]
use kithara_stream::Writer;
use kithara_stream::{Backend, NullStreamContext, StreamContext, StreamType, Timeline};
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

    fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(timeline))
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
            .cancel(cancel)
            .build();

        let key = kithara_assets::ResourceKey::absolute(path);
        let res = store.open_resource(&key).map_err(SourceError::Assets)?;
        let len = res.len();

        let bus = config
            .bus
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let timeline = Timeline::new();
        timeline.set_total_bytes(len);
        let progress = Arc::new(Progress::new(timeline.clone()));
        // Local file is fully available — mark download as complete.
        let total = len.unwrap_or(0);
        progress.set_download_pos(total);
        bus.publish(FileEvent::DownloadComplete { total_bytes: total });

        Ok(FileSource::new(res, progress, bus))
    }

    /// Create a source for a remote file (HTTP/HTTPS).
    ///
    /// Avoids a HEAD request because it can hang in browser environments.
    ///
    /// On native targets we open a streaming GET and reuse it in the downloader
    /// to avoid a second HTTP connection. On wasm32 we skip pre-open and let
    /// the downloader open the stream inside its backend Worker.
    async fn create_remote(
        url: url::Url,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, SourceError> {
        let asset_root = asset_root_for_url(&url, config.name.as_deref());
        let net_client = HttpClient::new(config.net.clone());

        #[cfg(not(target_arch = "wasm32"))]
        let byte_stream = net_client
            .stream(url.clone(), config.headers.clone())
            .await?;

        #[cfg(not(target_arch = "wasm32"))]
        let expected_len = byte_stream
            .headers
            .get("content-length")
            .or_else(|| byte_stream.headers.get("Content-Length"))
            .and_then(|value| value.parse::<u64>().ok());

        #[cfg(target_arch = "wasm32")]
        let expected_len = None;

        let mut backend_builder = AssetStoreBuilder::new()
            .root_dir(&config.store.cache_dir)
            .asset_root(Some(asset_root.as_str()))
            .evict_config(config.store.to_evict_config())
            .ephemeral(config.store.ephemeral)
            .cancel(cancel.clone());
        let uses_memory_backend = cfg!(target_arch = "wasm32") || config.store.ephemeral;
        if uses_memory_backend
            && let Some(capacity) = expected_len.and_then(|len| usize::try_from(len).ok())
        {
            backend_builder = backend_builder.mem_resource_capacity(capacity);
        }
        let backend = Arc::new(backend_builder.build());

        let state = FileStreamState::create(
            &backend,
            url,
            cancel.clone(),
            config.bus.clone(),
            config.event_channel_capacity,
            config.headers.clone(),
            expected_len,
        )?;

        let timeline = state.timeline();
        let progress = Arc::new(Progress::new(timeline));
        let shared = Arc::new(SharedFileState::new());

        // Determine if the resource is a complete cache or needs downloading.
        let is_partial = match state.res().status() {
            ResourceStatus::Committed { final_len } => {
                // File on disk might be smaller than expected Content-Length → partial.
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

            // Create downloader with shared state.
            let downloader = FileDownloader::new(
                &net_client,
                &state,
                progress.clone(),
                state.bus().clone(),
                config.look_ahead_bytes,
                shared.clone(),
            );

            #[cfg(not(target_arch = "wasm32"))]
            let downloader = downloader.with_initial_writer(Writer::new(
                byte_stream,
                state.res().clone(),
                cancel.clone(),
            ));

            #[cfg(target_arch = "wasm32")]
            let downloader = downloader;

            // Spawn downloader on a dedicated thread.
            // Backend is stored in FileSource — dropping the source cancels the downloader.
            let backend = Backend::new(downloader, &cancel);

            // Create source with shared state and backend for on-demand loading.
            let source = FileSource::with_shared(
                state.res().clone(),
                progress,
                state.bus().clone(),
                shared,
                backend,
            );

            return Ok(source);
        }

        // Fully cached — create source without backend (no downloader needed).
        let source = FileSource::new(state.res().clone(), progress, state.bus().clone());

        Ok(source)
    }
}
