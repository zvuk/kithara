use std::{path::PathBuf, sync::Arc};

use kithara_assets::{AssetStoreBuilder, ResourceKey, asset_root_for_url};
use kithara_events::EventBus;
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{
    SourceError as StreamSourceError, StreamType, Timeline,
    dl::{Downloader, DownloaderConfig},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{FileConfig, FileSrc},
    coord::FileCoord,
    error::SourceError,
    session::{
        FileAssetCtx, FileInner, FilePeer, FilePhase, FileSource, FileSourceCtx, FileStreamState,
    },
};

/// Marker type for file streaming.
pub struct File;

impl StreamType for File {
    type Config = FileConfig;
    type Events = EventBus;
    type Source = FileSource;

    async fn create(config: Self::Config) -> Result<Self::Source, StreamSourceError> {
        let cancel = config.cancel.clone().unwrap_or_default();
        let src = config.src.clone();

        std::future::ready(match src {
            FileSrc::Local(path) => Self::create_local(path, config, &cancel),
            FileSrc::Remote(url) => Self::create_remote(url, config, cancel),
        })
        .await
        .map_err(StreamSourceError::from)
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }
}

impl File {
    /// Create a source for a local file.
    fn create_local(
        path: PathBuf,
        config: FileConfig,
        cancel: &CancellationToken,
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
                .cancel(cancel.clone())
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

        Ok(FileSource::local(
            res,
            coord,
            bus,
            store,
            key,
            cancel.child_token(),
        ))
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
        let from_config = config.name.as_deref();
        let from_query = url.query();
        let name_or_query = from_config.or(from_query).filter(|s| !s.is_empty());
        let asset_root = asset_root_for_url(&url, name_or_query);

        let downloader = config.downloader.clone().unwrap_or_else(|| {
            Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()))
        });

        let backend_builder = AssetStoreBuilder::new()
            .root_dir(&config.store.cache_dir)
            .asset_root(Some(asset_root.as_str()))
            .evict_config(config.store.to_evict_config())
            .ephemeral(config.store.is_ephemeral)
            .cancel(cancel.clone());
        let backend = Arc::new(backend_builder.build());

        let state = FileStreamState::create(
            &backend,
            &url,
            config.bus.clone(),
            config.event_channel_capacity,
        )?;

        let timeline = Timeline::new();
        let coord = Arc::new(FileCoord::new(timeline));
        coord.set_total_bytes(state.res.len());

        if matches!(state.res.status(), ResourceStatus::Committed { .. }) {
            tracing::debug!("file already cached, skipping download");
            let total = coord.total_bytes().unwrap_or(0);
            coord.set_download_pos(total);

            return Ok(FileSource::local(
                state.res.clone(),
                coord,
                state.bus.clone(),
                Arc::clone(&state.backend),
                state.key.clone(),
                cancel.child_token(),
            ));
        }

        let inner = Arc::new(FileInner::new(
            FileSourceCtx {
                coord: Arc::clone(&coord),
                cancel,
                bus: state.bus.clone(),
            },
            FileAssetCtx {
                headers: config.headers,
                url,
                backend: Arc::clone(&state.backend),
                res: state.res.clone(),
                key: state.key.clone(),
            },
            FilePhase::Init,
        ));

        let _peer_handle = downloader
            .register(Arc::new(FilePeer::new(Arc::clone(&inner))))
            .with_bus(state.bus.clone());

        Ok(FileSource::from_inner(inner, coord))
    }
}
