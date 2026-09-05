use std::{marker::PhantomData, path::PathBuf};

use kithara_assets::{
    AcquisitionResult, AssetReader, AssetResource, AssetSource, AssetStore, AssetsError, ReadSide,
    ResourceAttachment, ResourceKey,
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_events::{EventBus, FileError, FileEvent};
use kithara_net::{Headers, HttpClient, NetOptions};
use kithara_platform::{CancelScope, CancelToken, sync::Arc, time::sleep, tokio};
use kithara_storage::StorageError;
use kithara_stream::{
    PlayheadState, SeekState, SourceError as StreamSourceError, StreamType,
    dl::{Downloader, DownloaderConfig},
};
use kithara_test_utils::kithara;
use url::Url;

use crate::{
    config::{FileConfig, FileSrc},
    coord::FileCoord,
    error::SourceError,
    session::{
        FileAssetCtx, FileInner, FileLocalConfig, FilePeer, FileSource, FileSourceCtx, sniff_codec,
    },
};

/// Marker type for file streaming.
pub struct File<S>(PhantomData<fn() -> S>);

struct Consts;

impl Consts {
    const DEFAULT_EXTENSION: &'static str = "bin";
    const MAX_EXTENSION_LEN: usize = 16;
}

#[derive(Default)]
struct TmpClaimProgress {
    last_len: Option<Option<u64>>,
}

impl TmpClaimProgress {
    const fn observe(&mut self, len: Option<u64>) -> bool {
        let stalled = matches!(
            (self.last_len, len),
            (Some(Some(previous)), Some(current)) if previous == current
        );
        self.last_len = Some(len);
        stalled
    }
}

struct RemoteFileOpen {
    coord: Arc<FileCoord>,
    cancel: CancelToken,
    downloader: Downloader,
    bus: EventBus,
    headers: Option<Headers>,
    url: Url,
    reader_event_capacity: usize,
}

fn local_key(path: PathBuf) -> Result<ResourceKey, SourceError> {
    if !path.is_absolute() {
        return Err(SourceError::InvalidPath(format!(
            "path must be absolute: {}",
            path.display()
        )));
    }
    if path.exists() {
        return ResourceKey::absolute(path).map_err(SourceError::from);
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

fn cached_source<S>(
    reader: AssetReader<S>,
    bus: EventBus,
    cancel: CancelToken,
    reader_event_capacity: usize,
) -> FileSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let coord = completed_coord(reader.len());
    let cached_codec = sniff_codec(&reader);
    FileSource::local(
        FileLocalConfig::builder()
            .reader(reader)
            .coord(coord)
            .bus(bus)
            .cancel(cancel)
            .reader_event_capacity(reader_event_capacity)
            .maybe_cached_codec(cached_codec)
            .build(),
    )
}

fn publish_open_error(bus: Option<&EventBus>, error: &SourceError) {
    if let Some(bus) = bus {
        bus.publish(FileEvent::Error {
            error: FileError::Io(error.to_string()),
        });
    }
}

fn valid_extension(extension: &str) -> Option<String> {
    let extension = extension.strip_prefix('.').unwrap_or(extension);
    (!extension.is_empty()
        && extension.len() <= Consts::MAX_EXTENSION_LEN
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    .then(|| extension.to_ascii_lowercase())
}

fn source_extension(url: &Url, hint: Option<&str>) -> String {
    hint.and_then(valid_extension)
        .or_else(|| {
            url.path_segments()
                .and_then(Iterator::last)
                .and_then(|leaf| leaf.rsplit_once('.'))
                .filter(|(stem, _)| !stem.is_empty())
                .and_then(|(_, extension)| valid_extension(extension))
        })
        .unwrap_or_else(|| Consts::DEFAULT_EXTENSION.to_string())
}

fn remote_key<S>(
    store: &AssetStore<S>,
    url: &Url,
    discriminator: Option<String>,
    extension: Option<&str>,
) -> Result<ResourceKey, SourceError>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let source = AssetSource::Remote {
        discriminator,
        url: url.clone(),
    };
    let scope = store.scope::<File<S>>(&source)?;
    let resource = AssetResource::Source {
        extension: source_extension(url, extension),
    };
    scope.key(&resource).map_err(SourceError::from)
}

fn default_downloader<S>(cancel: &CancelToken, pools: PoolRegion<S>) -> Downloader
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let cancel_for_dl = cancel.child();
    let client = HttpClient::new(NetOptions::default(), pools, cancel_for_dl.child());
    Downloader::new(
        DownloaderConfig::for_client(client)
            .cancel(cancel_for_dl)
            .build(),
    )
}

impl RemoteFileOpen {
    fn into_source<S>(self, attachment: ResourceAttachment<S>) -> FileSource<S>
    where
        S: HasPool<u8> + Send + Sync + 'static,
    {
        let Self {
            bus,
            cancel,
            coord,
            downloader,
            headers,
            reader_event_capacity,
            url,
        } = self;

        let (reader, resource_lease, writer) = attachment.into();
        if let Some(len) = reader.len() {
            coord.set_download_pos(len);
        }

        let inner = Arc::new(FileInner::new(
            FileSourceCtx {
                cancel,
                reader_event_capacity,
                coord: Arc::clone(&coord),
                bus: bus.clone(),
            },
            FileAssetCtx {
                reader,
                headers,
                url,
            },
            false,
            Some(resource_lease),
        ));

        let peer_handle = downloader
            .register(Arc::new(FilePeer::new(&inner, writer)))
            .with_bus(bus);

        inner.arm_reader_waker();
        let mut source = FileSource::with_inner(inner, coord);
        source.set_peer_handle(peer_handle);
        source
    }
}

impl<S> StreamType for File<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    type Config = FileConfig<S>;
    type Events = EventBus;
    type Source = FileSource<S>;

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

impl<S> File<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Create a source for a local file.
    fn create_local(
        path: PathBuf,
        config: FileConfig<S>,
        cancel: &CancelToken,
    ) -> Result<FileSource<S>, SourceError> {
        let key = local_key(path)?;
        let store = config.store.clone();
        let reader_event_capacity = config.reader_event_capacity;
        let bus = config
            .bus
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));
        let reader = store.open_resource(&key, None).map_err(|error| {
            let source_error = SourceError::Assets(error);
            publish_open_error(Some(&bus), &source_error);
            source_error
        })?;

        Ok(cached_source(
            reader,
            bus,
            cancel.child(),
            reader_event_capacity,
        ))
    }

    /// Create a source for a remote file.
    ///
    /// Registers the source with the [`Downloader`] and returns immediately.
    /// Content-Length and Content-Type are discovered asynchronously via the
    /// `on_connect` callback when the HTTP response arrives. Until then,
    /// `len()` returns `None`.
    fn create_remote(
        url: Url,
        config: FileConfig<S>,
        cancel: CancelToken,
    ) -> Result<FileSource<S>, SourceError> {
        let FileConfig {
            bus,
            discriminator,
            downloader,
            headers,
            pools,
            store,
            event_channel_capacity,
            extension,
            look_ahead_bytes,
            reader_event_capacity,
            ..
        } = config;
        let downloader = downloader.unwrap_or_else(|| default_downloader(&cancel, pools));
        let backend = store;
        let key = remote_key(&backend, &url, discriminator, extension.as_deref())?;
        let publish_bus = bus.clone();
        let bus = bus.unwrap_or_else(|| EventBus::new(event_channel_capacity));
        let coord = coord_with_total(None);
        let acq = backend
            .attach_pending_resource(&key, coord.read_pos_handle(), look_ahead_bytes)
            .map_err(SourceError::Assets)
            .inspect_err(|error| publish_open_error(publish_bus.as_ref(), error))?;

        match acq {
            AcquisitionResult::Ready(reader) => {
                tracing::debug!("file already cached, skipping download");
                Ok(cached_source(
                    reader,
                    bus,
                    cancel.child(),
                    reader_event_capacity,
                ))
            }
            AcquisitionResult::Pending(attachment) => Ok(RemoteFileOpen {
                coord,
                cancel,
                downloader,
                bus,
                headers,
                url,
                reader_event_capacity,
            }
            .into_source(attachment)),
            _ => Err(SourceError::UnexpectedAcquisitionState),
        }
    }

    /// Wait for a sibling `AssetStore` to release the atomic-chunked
    /// tmp file, then open. The sibling owner signals release either by
    /// committing (canonical appears) or by dropping without commit
    /// (tmp disappears) - both unblock our next
    /// `OpenOptions::create_new` call.
    ///
    /// `TmpClaimed` only ever names a *live* holder - a crashed-out
    /// process releases its advisory lock to the OS, and the next
    /// `AtomicChunked::open` reclaims that tmp - so this loop is
    /// guaranteed to terminate. `#[kithara::hang_watchdog]` still covers
    /// a live sibling that stops making progress: it keeps writing, so
    /// the tmp grows, and only a frozen tmp counts as no-progress -
    /// otherwise a second consumer of the same URL (e.g. waveform
    /// analysis alongside the player) would panic on any download
    /// longer than the watchdog timeout.
    #[kithara::hang_watchdog]
    async fn create_remote_wait_for_claim(
        url: Url,
        config: FileConfig<S>,
        cancel: CancelToken,
    ) -> Result<FileSource<S>, StreamSourceError> {
        let poll_interval = config.tmp_claim_poll_interval;
        let mut progress = TmpClaimProgress::default();
        loop {
            if cancel.is_cancelled() {
                return Err(StreamSourceError::Cancelled);
            }
            match Self::create_remote(url.clone(), config.clone(), cancel.clone()) {
                Ok(src) => {
                    hang_reset!();
                    return Ok(src);
                }
                Err(SourceError::Assets(AssetsError::Storage(StorageError::TmpClaimed(tmp)))) => {
                    let len = std::fs::metadata(&tmp).ok().map(|m| m.len());
                    if progress.observe(len) {
                        hang_tick!();
                    } else {
                        hang_reset!();
                    }
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err(StreamSourceError::Cancelled),
                        () = sleep(poll_interval) => {}
                    }
                }
                Err(e) => return Err(StreamSourceError::from(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_assets::{AcquisitionResult, AssetStore, StorageBackend};
    use kithara_events::{Event, FileEvent};
    use kithara_platform::time::Duration;
    use tempfile::tempdir;

    use super::*;
    use crate::test_pools::pools;

    fn url(value: &str) -> Url {
        Url::parse(value).expect("valid test URL")
    }

    #[kithara::test]
    #[case("https://example.com/audio.MP3?token=secret", "mp3")]
    #[case("https://example.com/archive.tar.gz", "gz")]
    #[case("https://example.com/audio", "bin")]
    #[case("https://example.com/.mp3", "bin")]
    #[case("https://example.com/audio.thisextensionistoolong", "bin")]
    #[case("https://example.com/audio.m%2F4a", "bin")]
    fn source_extension_uses_safe_final_url_extension(#[case] value: &str, #[case] expected: &str) {
        assert_eq!(source_extension(&url(value), None), expected);
    }

    #[kithara::test]
    fn source_extension_prefers_explicit_safe_hint() {
        assert_eq!(
            source_extension(&url("https://example.com/audio.bin"), Some("FLAC")),
            "flac"
        );
    }

    #[kithara::test]
    fn remote_key_uses_file_source_layout() {
        let store = AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build();
        let key = remote_key(
            &store,
            &url("https://example.com/get/audio.MP3?token=secret"),
            Some("track-42".to_string()),
            None,
        )
        .expect("remote key");

        assert_eq!(key.rel_path(), Some("track/track.mp3"));
    }

    #[kithara::test]
    fn remote_key_uses_only_explicit_discriminator() {
        let store = AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build();
        let first = remote_key(
            &store,
            &url("https://example.com/audio.mp3?token=first"),
            None,
            None,
        )
        .expect("first key");
        let second = remote_key(
            &store,
            &url("https://example.com/audio.mp3?token=second"),
            None,
            None,
        )
        .expect("second key");
        let named = remote_key(
            &store,
            &url("https://example.com/audio.mp3?token=second"),
            Some("named".to_string()),
            None,
        )
        .expect("named key");

        assert_eq!(first.asset_root(), second.asset_root());
        assert_ne!(first.asset_root(), named.asset_root());
    }

    #[kithara::test]
    fn local_key_rejects_relative_paths_as_invalid_paths() {
        let result = local_key(PathBuf::from("relative/track.mp3"));

        assert!(matches!(result, Err(SourceError::InvalidPath(_))));
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn remote_claim_wait_returns_cancelled() {
        let dir = tempdir().expect("test cache directory");
        let backend = StorageBackend::Disk {
            root: dir.path().to_path_buf(),
        };
        let pools = pools();
        let holder_store = AssetStore::builder(pools.clone())
            .backend(backend.clone())
            .build();
        let waiting_store = AssetStore::builder(pools.clone()).backend(backend).build();
        let remote_url = url("http://example.test/audio.mp3");
        let key = remote_key(&holder_store, &remote_url, None, None).expect("remote key");
        let holder = match holder_store
            .acquire_resource(&key, None)
            .expect("holder acquires resource")
        {
            AcquisitionResult::Pending(writer) => writer,
            AcquisitionResult::Ready(_) => panic!("holder must own the pending claim"),
            _ => panic!("holder returned an unexpected acquisition state"),
        };
        let scope = CancelScope::new(None);
        let cancel = scope.token();
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let config = FileConfig::for_src(FileSrc::Remote(remote_url))
            .store(waiting_store)
            .pools(pools)
            .cancel(cancel.clone())
            .events(bus)
            .build();
        let create = File::create(config);
        kithara_platform::tokio::pin!(create);

        loop {
            kithara_platform::tokio::select! {
                _result = &mut create => panic!("claim wait returned before cancellation"),
                event = events.recv() => {
                    let event = event.expect("event channel remains open");
                    if matches!(event.event, Event::File(FileEvent::Error { .. })) {
                        break;
                    }
                }
            }
        }

        scope.cancel();
        let result = create.await;

        assert!(matches!(result, Err(StreamSourceError::Cancelled)));
        drop(holder);
    }

    #[kithara::test]
    fn absent_first_tmp_observation_is_not_a_stall() {
        let mut progress = TmpClaimProgress::default();

        assert!(!progress.observe(None));
    }
}
