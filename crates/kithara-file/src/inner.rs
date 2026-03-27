//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait.

use std::{path::PathBuf, sync::Arc};

use kithara_assets::{AssetStoreBuilder, ResourceKey, asset_root_for_url};
use kithara_events::{EventBus, FileEvent};
use kithara_net::HttpClient;
use kithara_storage::{ResourceExt, ResourceStatus};
#[cfg(not(target_arch = "wasm32"))]
use kithara_stream::Writer;
use kithara_stream::{AudioCodec, Backend, StreamType, Timeline};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{FileConfig, FileSrc},
    coord::FileCoord,
    downloader::FileDownloader,
    error::SourceError,
    session::{FileSource, FileStreamState},
};

/// Marker type for file streaming.
pub struct File;

impl StreamType for File {
    type Config = FileConfig;
    type Coord = Arc<FileCoord>;
    type Demand = std::ops::Range<u64>;
    type Source = FileSource;
    type Error = SourceError;
    type Events = EventBus;
    type Layout = ();
    type Topology = ();

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
}

impl File {
    /// Create a source for a local file.
    ///
    /// Opens the file via `AssetStore` with an absolute `ResourceKey`,
    /// skipping network and background downloader entirely.
    fn create_local(
        path: PathBuf,
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

        let key = ResourceKey::absolute(path);
        let res = store.open_resource(&key).map_err(SourceError::Assets)?;
        let len = res.len();

        let bus = config
            .bus
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let timeline = Timeline::new();
        let coord = Arc::new(FileCoord::new(timeline.clone()));
        coord.set_total_bytes(len);
        // Local file is fully available — mark download as complete.
        let total = len.unwrap_or(0);
        coord.set_download_pos(total);
        bus.publish(FileEvent::DownloadComplete { total_bytes: total });

        Ok(FileSource::new(res, coord, bus))
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
        // Include query params in asset root so URLs differing only in query
        // (e.g. ?id=123 vs ?id=456) get distinct cache directories.
        let name_or_query = config
            .name
            .as_deref()
            .or_else(|| url.query())
            .filter(|s| !s.is_empty());
        let asset_root = asset_root_for_url(&url, name_or_query);
        let net_client = HttpClient::new({
            let mut opts = config.net.clone();
            if let Some(ref bus) = config.bus {
                let bus = bus.clone();
                opts.on_slow = Some(Arc::new(move || bus.publish(FileEvent::LoadSlow)));
            }
            opts
        });

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

        #[cfg(not(target_arch = "wasm32"))]
        let content_type_codec = byte_stream
            .headers
            .get("content-type")
            .or_else(|| byte_stream.headers.get("Content-Type"))
            .and_then(AudioCodec::from_mime);

        #[cfg(target_arch = "wasm32")]
        let expected_len = None;

        #[cfg(target_arch = "wasm32")]
        let content_type_codec = None;

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
        )?;

        let timeline = Timeline::new();
        let coord = Arc::new(FileCoord::new(timeline));
        coord.set_total_bytes(expected_len.or_else(|| state.res().len()));

        // Determine if the resource is a complete cache or needs downloading.
        let is_partial = match state.res().status() {
            ResourceStatus::Committed { final_len } => {
                // File on disk might be smaller than expected Content-Length → partial.
                expected_len
                    .zip(final_len)
                    .is_some_and(|(expected, actual)| actual < expected)
            }
            _ => false,
        };

        if matches!(state.res().status(), ResourceStatus::Committed { .. }) && !is_partial {
            // Fully cached — no download needed.
            tracing::debug!("file already cached, skipping download");
            let total = coord.total_bytes().unwrap_or(0);
            coord.set_download_pos(total);
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
                Arc::clone(&coord),
                state.bus().clone(),
                config.look_ahead_bytes,
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
            let backend = Backend::new(downloader, &cancel, config.runtime.clone());

            // Create source with shared state and backend for on-demand loading.
            let source =
                FileSource::with_backend(state.res().clone(), coord, state.bus().clone(), backend)
                    .with_content_type_codec(content_type_codec);

            return Ok(source);
        }

        // Fully cached — create source without backend (no downloader needed).
        let source = FileSource::new(state.res().clone(), coord, state.bus().clone())
            .with_content_type_codec(content_type_codec);

        Ok(source)
    }
}
