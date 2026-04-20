//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait.
//! All code is synchronous — async HTTP I/O is handled by the [`Downloader`].

use std::{path::PathBuf, sync::Arc};

use kithara_assets::{AssetStoreBuilder, ResourceKey, asset_root_for_url};
use kithara_events::{EventBus, FileEvent};
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{
    StreamType, Timeline,
    dl::{Downloader, DownloaderConfig},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{FileConfig, FileSrc},
    coord::FileCoord,
    error::SourceError,
    session::{FilePeer, FileSource, FileStreamState},
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

        // Fully synchronous — returned future is always ready.
        std::future::ready(match src {
            FileSrc::Local(path) => Self::create_local(path, config, cancel),
            FileSrc::Remote(url) => Self::create_remote(url, config, cancel),
        })
        .await
    }
}

impl File {
    /// Create a source for a local file.
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

        let store = Arc::new(
            AssetStoreBuilder::new()
                .asset_root(None)
                .cancel(cancel)
                .build(),
        );

        let key = ResourceKey::absolute(path);
        let res = store.open_resource(&key).map_err(SourceError::Assets)?;
        let len = res.len();

        let bus = config
            .bus
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let timeline = Timeline::new();
        let coord = Arc::new(FileCoord::new(timeline));
        coord.set_total_bytes(len);
        let total = len.unwrap_or(0);
        coord.set_download_pos(total);
        bus.publish(FileEvent::DownloadComplete { total_bytes: total });

        Ok(FileSource::local(res, coord, bus, store, key))
    }

    /// Create a source for a remote file.
    ///
    /// Registers the source with the [`Downloader`] and returns immediately.
    /// Content-Length and Content-Type are discovered asynchronously via the
    /// `on_connect` callback when the HTTP response arrives. Until then,
    /// `len()` returns `None`.
    fn create_remote(
        url: url::Url,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, SourceError> {
        let name_or_query = config
            .name
            .as_deref()
            .or_else(|| url.query())
            .filter(|s| !s.is_empty());
        let asset_root = asset_root_for_url(&url, name_or_query);

        let downloader = config.downloader.clone().unwrap_or_else(|| {
            Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()))
        });

        let backend_builder = AssetStoreBuilder::new()
            .root_dir(&config.store.cache_dir)
            .asset_root(Some(asset_root.as_str()))
            .evict_config(config.store.to_evict_config())
            .ephemeral(config.store.ephemeral)
            .cancel(cancel.clone());
        if cfg!(target_arch = "wasm32") || config.store.ephemeral {
            // No pre-allocation — length unknown until on_connect.
        }
        let backend = Arc::new(backend_builder.build());

        let state = FileStreamState::create(
            &backend,
            &url,
            config.bus.clone(),
            config.event_channel_capacity,
        )?;

        let timeline = Timeline::new();
        let coord = Arc::new(FileCoord::new(timeline));
        coord.set_total_bytes(state.res.len()); // None for fresh resources

        // Check if already fully cached.
        if matches!(state.res.status(), ResourceStatus::Committed { .. }) {
            tracing::debug!("file already cached, skipping download");
            let total = coord.total_bytes().unwrap_or(0);
            coord.set_download_pos(total);
            state
                .bus
                .publish(FileEvent::DownloadComplete { total_bytes: total });

            return Ok(FileSource::local(
                state.res.clone(),
                coord,
                state.bus.clone(),
                Arc::clone(&state.backend),
                state.key.clone(),
            ));
        }

        // Register a peer and get a PeerHandle for HTTP requests.
        let peer_handle = downloader
            .register(Arc::new(FilePeer::new(coord.timeline())))
            .with_bus(state.bus.clone());

        // Create source — spawns download tasks internally.
        let source = FileSource::remote(
            &state,
            coord,
            cancel,
            url,
            config.headers,
            config.look_ahead_bytes,
            peer_handle,
        );

        Ok(source)
    }
}
