use std::{path::PathBuf, sync::Arc};

use kithara_assets::{
    AcquisitionResult, AssetReader, AssetStore, AssetStoreBuilder, AssetsError, EvictConfig,
    ReadSide, ResourceKey, StoreOptions, WriteSide, asset_root_for_url,
};
use kithara_events::EventBus;
use kithara_platform::{CancellationToken, Mutex, time::Duration, tokio};
use kithara_storage::StorageError;
use kithara_stream::{
    AudioCodec, SourceError as StreamSourceError, StreamType, Timeline,
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
        let reader = store
            .open_resource(&key, None)
            .map_err(SourceError::Assets)?;
        let len = reader.len();

        let bus = config
            .bus
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let timeline = Timeline::new();
        let coord = Arc::new(FileCoord::new(timeline));
        coord.set_total_bytes(len);
        let total = len.unwrap_or(0);
        coord.set_download_pos(total);

        let cached_codec = sniff_codec(&reader);

        Ok(FileSource::local(
            reader,
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
        let FileStreamState {
            backend,
            acq,
            bus,
            key,
        } = FileStreamState::create(
            &backend,
            key,
            config.bus.clone(),
            config.event_channel_capacity,
        )?;

        let timeline = Timeline::new();
        let coord = Arc::new(FileCoord::new(timeline));

        // `Ready` means the file is already committed in the cache — no
        // download. `Pending` hands the single non-Clone commit owner to the
        // download path. The phase replaces the old runtime `status()` probe.
        let writer = match acq {
            AcquisitionResult::Ready(reader) => {
                tracing::debug!("file already cached, skipping download");
                coord.set_total_bytes(reader.len());
                let total = coord.total_bytes().unwrap_or(0);
                coord.set_download_pos(total);
                let cached_codec = sniff_codec(&reader);
                return Ok(FileSource::local(
                    reader,
                    coord,
                    bus,
                    backend,
                    key,
                    cancel.child_token(),
                    cached_codec,
                ));
            }
            AcquisitionResult::Pending(writer) => writer,
            _ => unreachable!("AcquisitionResult is Pending | Ready"),
        };

        let reader = writer.reader();
        let raw = writer.raw_write_handle();
        coord.set_total_bytes(reader.len());

        // Attach this consumer's download demand on the shared resource.
        // The CAS winner gets the producer handle and drives the GET; a
        // concurrent consumer of the same URL (e.g. a waveform analyzer)
        // loses and reads the shared bytes instead of issuing its own
        // download. With a private per-stream store this is a single
        // consumer that always wins.
        let (demand_lease, producer) =
            backend.attach_demand(&key, coord.read_pos_handle(), config.look_ahead_bytes);

        let inner = Arc::new(FileInner::new(
            FileSourceCtx {
                cancel,
                coord: Arc::clone(&coord),
                bus: bus.clone(),
            },
            FileAssetCtx {
                url,
                headers: config.headers,
                backend: Arc::clone(&backend),
                reader,
                writer: Mutex::new(Some(writer)),
                raw: Some(raw),
                key: key.clone(),
            },
            FilePhase::Init,
            Some(demand_lease),
        ));

        let peer_handle = downloader
            .register(Arc::new(FilePeer::new(Arc::clone(&inner), producer)))
            .with_bus(bus.clone());

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
        /// Bounded poll interval while a sibling `AssetStore` instance holds
        /// the atomic-chunked tmp for the same canonical path. Short enough
        /// that the observed ~67 ms race window in
        /// `local_queue_playlist_behavior` resolves in a handful of ticks but
        /// long enough not to busy-spin a tokio worker.
        const TMP_CLAIMED_POLL_INTERVAL: Duration = Duration::from_millis(10);
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
fn sniff_codec(reader: &AssetReader) -> Option<AudioCodec> {
    let mut buf = [0u8; 16];
    let read = reader.read_at(0, &mut buf).ok()?;
    AudioCodec::try_from(&buf[..read]).ok()
}
