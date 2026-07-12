use std::path::PathBuf;

use kithara_assets::{
    AcquisitionResult, AssetLayout, AssetReader, AssetStore, AssetStoreBuilder, AssetWriter,
    AssetsError, BytePool, EvictConfig, ReadSide, ResourceKey, StoreOptions, WriteSide,
};
use kithara_events::EventBus;
use kithara_net::{Headers, HttpClient, NetOptions};
use kithara_platform::{
    CancelScope, CancelToken,
    sync::{Arc, Mutex},
    time::{Duration, sleep},
};
use kithara_storage::StorageError;
use kithara_stream::{
    AudioCodec, PlayheadState, SeekState, SourceError as StreamSourceError, StreamType,
    dl::{Downloader, DownloaderConfig},
};
use kithara_test_utils::kithara;

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

struct RemoteFileOpen {
    backend: Arc<AssetStore>,
    cancel: CancelToken,
    downloader: Downloader,
    bus: EventBus,
    headers: Option<Headers>,
    look_ahead_bytes: Option<u64>,
    key: ResourceKey,
    url: url::Url,
}

fn local_key(path: PathBuf) -> Result<ResourceKey, SourceError> {
    if path.exists() {
        return Ok(ResourceKey::absolute(path));
    }
    Err(SourceError::InvalidPath(format!(
        "file not found: {}",
        path.display()
    )))
}

fn coord_with_total(len: Option<u64>) -> Arc<FileCoord> {
    let coord = Arc::new(FileCoord::new(
        Arc::new(PlayheadState::new()),
        Arc::new(SeekState::new()),
    ));
    coord.set_total_bytes(len);
    coord
}

fn completed_coord(len: Option<u64>) -> Arc<FileCoord> {
    let coord = coord_with_total(len);
    coord.set_download_pos(len.unwrap_or(0));
    coord
}

fn cached_source(
    reader: AssetReader,
    bus: EventBus,
    backend: Arc<AssetStore>,
    key: ResourceKey,
    cancel: CancelToken,
) -> FileSource {
    let coord = completed_coord(reader.len());
    let cached_codec = sniff_codec(&reader);
    FileSource::local(reader, coord, bus, backend, key, cancel, cached_codec)
}

fn remote_asset_root(url: &url::Url, name: Option<&str>, layout: &Arc<dyn AssetLayout>) -> String {
    let name_or_query = name
        .or_else(|| url.query())
        .filter(|value| !value.is_empty());
    layout.asset_root(url, name_or_query)
}

fn default_downloader(cancel: &CancelToken, pool: Option<BytePool>) -> Downloader {
    let cancel_for_dl = cancel.child();
    let net_options = NetOptions::builder().maybe_byte_pool(pool).build();
    let client = HttpClient::new(net_options, cancel_for_dl.child());
    Downloader::new(
        DownloaderConfig::for_client(client)
            .cancel(cancel_for_dl)
            .build(),
    )
}

impl RemoteFileOpen {
    fn into_source(self, writer: AssetWriter) -> FileSource {
        let Self {
            backend,
            bus,
            cancel,
            downloader,
            headers,
            key,
            look_ahead_bytes,
            url,
        } = self;

        let reader = writer.reader();
        let raw = writer.raw_write_handle();
        let coord = coord_with_total(reader.len());
        let (demand_lease, producer) =
            backend.attach_demand(&key, coord.read_pos_handle(), look_ahead_bytes);

        let inner = Arc::new(FileInner::new(
            FileSourceCtx {
                cancel,
                coord: Arc::clone(&coord),
                bus: bus.clone(),
            },
            FileAssetCtx {
                url,
                reader,
                headers,
                backend,
                key,
                writer: Mutex::new(Some(writer)),
                raw: Some(raw),
            },
            FilePhase::Init,
            Some(demand_lease),
        ));

        let peer_handle = downloader
            .register(Arc::new(FilePeer::new(Arc::clone(&inner), producer)))
            .with_bus(bus);

        let mut source = FileSource::with_inner(inner, coord);
        source.set_peer_handle(peer_handle);
        source
    }
}

impl StreamType for File {
    type Config = FileConfig;
    type Events = EventBus;
    type Source = FileSource;

    async fn create(config: Self::Config) -> Result<Self::Source, StreamSourceError> {
        let cancel = CancelScope::new(config.cancel.clone()).token();
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
        cancel: &CancelToken,
    ) -> Result<FileSource, SourceError> {
        let key = local_key(path)?;
        let store = Arc::new(
            AssetStoreBuilder::default()
                .cancel(cancel.clone())
                .maybe_pool(config.pool.clone())
                .build(),
        );
        let reader = store
            .open_resource(&key, None)
            .map_err(SourceError::Assets)?;
        let bus = config
            .bus
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        Ok(cached_source(reader, bus, store, key, cancel.child()))
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
        cancel: CancelToken,
    ) -> Result<FileSource, SourceError> {
        let FileConfig {
            asset_store,
            bus,
            downloader,
            event_channel_capacity,
            headers,
            look_ahead_bytes,
            name,
            pool,
            store,
            ..
        } = config;
        let downloader = downloader.unwrap_or_else(|| default_downloader(&cancel, pool.clone()));
        let backend =
            asset_store.unwrap_or_else(|| build_shared_asset_store(&store, pool, cancel.clone()));
        let asset_root = remote_asset_root(&url, name.as_deref(), backend.layout());

        let key = backend.scope(asset_root.as_str()).key_for(&url);
        let FileStreamState {
            backend,
            acq,
            bus,
            key,
        } = FileStreamState::create(&backend, key, bus, event_channel_capacity)?;

        // `Ready` means the file is already committed in the cache — no
        // download. `Pending` hands the single non-Clone commit owner to the
        // download path. The phase replaces the old runtime `status()` probe.
        match acq {
            AcquisitionResult::Ready(reader) => {
                tracing::debug!("file already cached, skipping download");
                Ok(cached_source(reader, bus, backend, key, cancel.child()))
            }
            AcquisitionResult::Pending(writer) => Ok(RemoteFileOpen {
                backend,
                cancel,
                downloader,
                bus,
                headers,
                look_ahead_bytes,
                key,
                url,
            }
            .into_source(writer)),
            _ => Err(SourceError::UnexpectedAcquisitionState),
        }
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
    /// rather than an indefinite hang. A *live* sibling keeps writing,
    /// so the tmp grows; only a frozen tmp counts as no-progress —
    /// otherwise a second consumer of the same URL (e.g. waveform
    /// analysis alongside the player) would panic on any download
    /// longer than the watchdog timeout.
    #[kithara::hang_watchdog]
    async fn create_remote_wait_for_claim(
        url: url::Url,
        config: FileConfig,
        cancel: CancelToken,
    ) -> Result<FileSource, StreamSourceError> {
        /// Bounded poll interval while a sibling `AssetStore` instance holds
        /// the atomic-chunked tmp for the same canonical path. Short enough
        /// that the observed ~67 ms race window in
        /// `local_queue_playlist_behavior` resolves in a handful of ticks but
        /// long enough not to busy-spin a tokio worker.
        const TMP_CLAIMED_POLL_INTERVAL: Duration = Duration::from_millis(10);
        let mut last_len: Option<u64> = None;
        loop {
            match Self::create_remote(url.clone(), config.clone(), cancel.clone()) {
                Ok(src) => {
                    hang_reset!();
                    return Ok(src);
                }
                Err(SourceError::Assets(AssetsError::Storage(StorageError::TmpClaimed(tmp)))) => {
                    let len = std::fs::metadata(&tmp).ok().map(|m| m.len());
                    if len == last_len {
                        hang_tick!();
                    } else {
                        last_len = len;
                        hang_reset!();
                    }
                    sleep(TMP_CLAIMED_POLL_INTERVAL).await;
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
/// master so a shutdown cascades through the store. Also used as the
/// standalone default when no store is injected (single consumer).
#[must_use]
pub(crate) fn build_shared_asset_store(
    store: &StoreOptions,
    pool: Option<BytePool>,
    cancel: CancelToken,
) -> Arc<AssetStore> {
    Arc::new(
        AssetStoreBuilder::default()
            .cancel(cancel)
            .backend(store.backend.clone())
            .evict_config(EvictConfig::from(store))
            .maybe_layout(store.layout.clone())
            .maybe_pool(pool)
            .maybe_cache_capacity(store.cache_capacity)
            .maybe_flush_hub(store.flush_hub.clone())
            .build(),
    )
}

/// Probe the first bytes of a committed `AssetResource` and try to
/// classify the codec by magic prefix. Returns `None` when the read
/// itself fails or the prefix doesn't match a known signature — callers
/// must treat both as "no hint available" and fall back to the regular
/// probe path.
fn sniff_codec(reader: &AssetReader) -> Option<AudioCodec> {
    let mut buf = [0u8; 16];
    let read = reader.read_at(0, &mut buf).ok()?;
    AudioCodec::try_from(&buf[..read]).ok()
}
