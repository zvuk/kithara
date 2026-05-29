use std::{path::PathBuf, sync::Arc};

use kithara_assets::{
    AssetResource, AssetStore, AssetStoreBuilder, AssetsError, EvictConfig, ResourceKey,
    StoreOptions, asset_root_for_url,
};
use kithara_events::EventBus;
use kithara_platform::{time::Duration, tokio};
use kithara_storage::{ResourceExt, ResourceStatus, StorageError};
use kithara_stream::{
    AudioCodec, SourceError as StreamSourceError, StreamType, Timeline,
    dl::{Downloader, DownloaderConfig},
};
use kithara_test_utils::kithara;
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

/// Bounded poll interval while a sibling `AssetStore` instance holds
/// the atomic-chunked tmp for the same canonical path. Short enough
/// that the observed ~67 ms race window in
/// `local_queue_playlist_behavior` resolves in a handful of ticks but
/// long enough not to busy-spin a tokio worker.
const TMP_CLAIMED_POLL_INTERVAL: Duration = Duration::from_millis(10);

impl StreamType for File {
    type Config = FileConfig;
    type Events = EventBus;
    type Source = FileSource;

    async fn create(config: Self::Config) -> Result<Self::Source, StreamSourceError> {
        let cancel = config.cancel.clone().unwrap_or_default();
        let src = config.src.clone();

        match src {
            FileSrc::Local(path) => {
                Self::create_local(path, config, &cancel).map_err(StreamSourceError::from)
            }
            FileSrc::Remote(url) => Self::create_remote_wait_for_claim(url, config, cancel).await,
        }
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

        let store = Arc::new(AssetStoreBuilder::new().cancel(cancel.clone()).build());

        let key = ResourceKey::absolute(path);
        let res = store
            .open_resource(&key, None)
            .map_err(SourceError::Assets)?;
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

        let backend = config
            .asset_store
            .clone()
            .unwrap_or_else(|| build_shared_asset_store(&config.store, cancel.clone()));

        let key = backend.scope(asset_root.as_str()).key_from_url(&url);
        let state = FileStreamState::create(
            &backend,
            key,
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

        // Attach this consumer's download demand on the shared resource.
        // The CAS winner gets the producer handle and drives the GET; a
        // concurrent consumer of the same URL (e.g. a waveform analyzer)
        // loses and reads the shared bytes instead of issuing its own
        // download. With a private per-stream store this is a single
        // consumer that always wins.
        let (demand_lease, producer) = state.backend.attach_demand(
            &state.key,
            coord.read_pos_handle(),
            config.look_ahead_bytes,
        );

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
            Some(demand_lease),
        ));

        let peer_handle = downloader
            .register(Arc::new(FilePeer::new(Arc::clone(&inner), producer)))
            .with_bus(state.bus.clone());

        let mut source = FileSource::with_inner(inner, coord);
        source.set_peer_handle(peer_handle);
        Ok(source)
    }

    /// Wait for a sibling `AssetStore` to release the atomic-chunked
    /// tmp file, then open. The sibling owner signals release either by
    /// committing (canonical appears) or by dropping without commit
    /// (tmp disappears) — both unblock our next
    /// `OpenOptions::create_new` call.
    ///
    /// Wrapped in `#[kithara::hang_watchdog]` so a stale tmp from a
    /// crashed-out previous process (which never releases the
    /// filesystem-level signal) surfaces as a deterministic panic
    /// rather than an indefinite hang.
    #[kithara::hang_watchdog]
    async fn create_remote_wait_for_claim(
        url: url::Url,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, StreamSourceError> {
        loop {
            match Self::create_remote(url.clone(), config.clone(), cancel.clone()) {
                Ok(src) => {
                    hang_reset!();
                    return Ok(src);
                }
                Err(SourceError::Assets(AssetsError::Storage(StorageError::TmpClaimed(_)))) => {
                    hang_tick!();
                    tokio::time::sleep(TMP_CLAIMED_POLL_INTERVAL).await;
                }
                Err(e) => return Err(StreamSourceError::from(e)),
            }
        }
    }
}

/// Build an app-wide shared file asset store from [`StoreOptions`].
///
/// Inject the result into every [`FileConfig::asset_store`](crate::FileConfig)
/// that should cooperate on a single cache so concurrent consumers of
/// one URL share a single download. `cancel` must be a child of the app
/// master so a shutdown cascades through the store. Also the standalone
/// fallback when no store is injected (single consumer).
#[must_use]
pub fn build_shared_asset_store(
    store: &StoreOptions,
    cancel: CancellationToken,
) -> Arc<AssetStore<()>> {
    let mut builder = AssetStoreBuilder::new()
        .cancel(cancel)
        .root_dir(&store.cache_dir)
        .evict_config(EvictConfig::from(store))
        .ephemeral(store.is_ephemeral);
    if let Some(cap) = store.cache_capacity {
        builder = builder.cache_capacity(cap);
    }
    if let Some(ref hub) = store.flush_hub {
        builder = builder.flush_hub(Arc::clone(hub));
    }
    Arc::new(builder.build())
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
