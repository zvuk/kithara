use std::{path::PathBuf, sync::Arc};

use kithara_assets::{AssetResource, AssetStoreBuilder, ResourceKey, asset_root_for_url};
use kithara_events::EventBus;
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{
    AudioCodec, SourceError as StreamSourceError, StreamType, Timeline,
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

        let cached_codec = sniff_codec(&res);

        Ok(FileSource::local(
            res,
            coord,
            bus,
            store,
            key,
            cancel.child_token(),
            cached_codec,
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
            let cancel_for_dl = cancel.child_token();
            let client = kithara_net::HttpClient::new(
                kithara_net::NetOptions::default(),
                cancel_for_dl.child_token(),
            );
            Downloader::new(
                DownloaderConfig::for_client(client)
                    .cancel(cancel_for_dl)
                    .build(),
            )
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

            // The HTTP `Content-Type` that originally drove
            // `FilePeer::capture_content_metadata` is gone on cold
            // restart — recover the codec by sniffing the first bytes
            // of the cached resource so the decoder doesn't fall back
            // to extension-based probing (which silently fails on
            // path-extension-less URLs like `streamhq?id=N`).
            let cached_codec = sniff_codec(&state.res);

            return Ok(FileSource::local(
                state.res.clone(),
                coord,
                state.bus.clone(),
                Arc::clone(&state.backend),
                state.key.clone(),
                cancel.child_token(),
                cached_codec,
            ));
        }

        let inner = Arc::new(FileInner::new(
            FileSourceCtx {
                cancel,
                coord: Arc::clone(&coord),
                bus: state.bus.clone(),
            },
            FileAssetCtx {
                url,
                headers: config.headers,
                backend: Arc::clone(&state.backend),
                res: state.res.clone(),
                key: state.key.clone(),
            },
            FilePhase::Init,
        ));

        let peer_handle = downloader
            .register(Arc::new(FilePeer::new(Arc::clone(&inner))))
            .with_bus(state.bus.clone());

        let mut source = FileSource::from_inner(inner, coord);
        source.set_peer_handle(peer_handle);
        Ok(source)
    }
}

/// Probe the first bytes of a committed `AssetResource` and try to
/// classify the codec by magic prefix. Returns `None` when the read
/// itself fails or the prefix doesn't match a known signature — callers
/// must treat both as "no hint available" and fall back to the regular
/// probe path.
fn sniff_codec(res: &AssetResource) -> Option<AudioCodec> {
    let mut buf = [0u8; 16];
    let read = res.read_at(0, &mut buf).ok()?;
    AudioCodec::try_from(&buf[..read]).ok()
}
