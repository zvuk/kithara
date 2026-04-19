#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use std::{
    fs,
    io::{self, Error as IoError, ErrorKind},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara_platform::tokio::{runtime::Handle, sync::Notify};
use kithara_storage::{
    Atomic, AvailabilityObserver, MmapOptions, MmapResource, OpenMode, Resource, ResourceExt,
    ResourceStatus, StorageError, StorageResource,
};
use tokio_util::sync::CancellationToken;

use crate::{
    base::{Assets, Capabilities},
    error::{AssetsError, AssetsResult},
    index::AvailabilityIndex,
    key::ResourceKey,
    state::AssetResourceState,
};

/// Initial mmap file size for index resources (4 KB).
const INDEX_INITIAL_SIZE: u64 = 4096;

/// Coordination primitive for auto-checkpointing `_index/availability.bin`.
///
/// Every [`ScopedAvailabilityObserver::on_commit`] call increments a shared
/// counter and wakes [`DiskAssetStore::spawn_auto_flush`]'s background task
/// every `threshold` commits. The task also flushes on cancel-token
/// shutdown.
#[derive(Debug)]
pub(crate) struct CheckpointSignal {
    pub(crate) commits: AtomicUsize,
    pub(crate) threshold: NonZeroUsize,
    pub(crate) notify: Notify,
}

impl CheckpointSignal {
    fn new(threshold: NonZeroUsize) -> Self {
        Self {
            commits: AtomicUsize::new(0),
            threshold,
            notify: Notify::new(),
        }
    }

    /// Called from the availability observer after every commit.
    pub(crate) fn on_commit(&self) {
        let prev = self.commits.fetch_add(1, Ordering::Relaxed);
        if (prev + 1).is_multiple_of(self.threshold.get()) {
            self.notify.notify_one();
        }
    }
}

/// Concrete on-disk [`Assets`] implementation for a single asset.
///
/// Maps [`ResourceKey`] to disk paths under a root directory.
/// Each `DiskAssetStore` is scoped to a single `asset_root`.
#[derive(Clone, Debug)]
pub struct DiskAssetStore {
    root_dir: PathBuf,
    asset_root: String,
    cancel: CancellationToken,
    availability: AvailabilityIndex,
    /// When set, every commit observed by this store increments a shared
    /// counter and — every `threshold` commits — wakes the background
    /// flusher spawned by [`DiskAssetStore::spawn_auto_flush`]. `None`
    /// preserves the explicit-checkpoint historical behaviour (callers
    /// must invoke `AssetStore::checkpoint()` themselves).
    checkpoint_signal: Option<Arc<CheckpointSignal>>,
}

impl DiskAssetStore {
    /// Create a store rooted at `root_dir` for a specific `asset_root`
    /// with its own unshared [`AvailabilityIndex`]. Convenient for
    /// tests and the `internal` feature surface; production
    /// construction (via `AssetStoreBuilder::build`) uses
    /// [`DiskAssetStore::with_availability`] to share the aggregate
    /// with the enum variant.
    pub fn new<P: Into<PathBuf>, S: Into<String>>(
        root_dir: P,
        asset_root: S,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_availability(root_dir, asset_root, cancel, AvailabilityIndex::new(), None)
    }

    /// Like [`DiskAssetStore::new`] but shares the given aggregate
    /// availability handle. Observer callbacks fired by this store's
    /// resources mutate the shared handle, so queries through the
    /// owning [`crate::AssetStore`] see the updates immediately.
    ///
    /// If `_index/availability.bin` exists under `root_dir`, the
    /// constructor best-effort seeds the shared [`AvailabilityIndex`]
    /// from it. A missing / empty / corrupt file is silently treated
    /// as an empty seed (same policy as `LruIndex::load` and
    /// `PinsIndex::load`). Errors from the underlying resource read
    /// itself are swallowed here — a broken cache must not prevent
    /// store construction.
    ///
    /// When `checkpoint_every` is `Some`, a [`CheckpointSignal`] is
    /// attached to the availability observer so commits can drive the
    /// background flusher (see [`Self::spawn_auto_flush`]). `None`
    /// disables auto-flush — callers must invoke
    /// [`crate::AssetStore::checkpoint`] explicitly.
    pub(crate) fn with_availability<P: Into<PathBuf>, S: Into<String>>(
        root_dir: P,
        asset_root: S,
        cancel: CancellationToken,
        availability: AvailabilityIndex,
        checkpoint_every: Option<NonZeroUsize>,
    ) -> Self {
        let checkpoint_signal = checkpoint_every.map(|n| Arc::new(CheckpointSignal::new(n)));
        let store = Self {
            root_dir: root_dir.into(),
            asset_root: asset_root.into(),
            cancel,
            availability,
            checkpoint_signal,
        };
        let _ = store.seed_availability_from_disk();
        store
    }

    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    #[must_use]
    pub fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn resource_path(&self, key: &ResourceKey) -> AssetsResult<PathBuf> {
        match key {
            ResourceKey::Relative(rel) => {
                let asset_root =
                    sanitize_rel(&self.asset_root).map_err(|()| AssetsError::InvalidKey)?;
                let rel_path = sanitize_rel(rel).map_err(|()| AssetsError::InvalidKey)?;
                Ok(self.root_dir.join(asset_root).join(rel_path))
            }
            ResourceKey::Absolute(path) => Ok(path.clone()),
        }
    }

    fn pins_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("pins.bin")
    }

    fn lru_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("lru.bin")
    }

    fn availability_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("availability.bin")
    }

    /// Open `_index/availability.bin` as a raw `MmapResource`. Used by
    /// [`Self::checkpoint`] and [`Self::seed_availability_from_disk`]
    /// to persist / hydrate the [`AvailabilityIndex`].
    fn open_availability_index_resource(&self) -> AssetsResult<MmapResource> {
        self.open_index_resource(self.availability_index_path())
    }

    /// Hydrate the shared [`AvailabilityIndex`] from disk if a
    /// persisted snapshot exists. Called exactly once, from
    /// [`Self::with_availability`] at construction time.
    ///
    /// If `_index/availability.bin` is absent, this is a no-op. A
    /// corrupt / wrong-version payload is silently dropped by
    /// `AvailabilityIndex::load_from` and the aggregate stays empty.
    fn seed_availability_from_disk(&self) -> AssetsResult<()> {
        if !self.availability_index_path().exists() {
            return Ok(());
        }
        let res = self.open_availability_index_resource()?;
        let atomic = Atomic::new(res);
        self.availability.load_from(&atomic)
    }

    /// Persist the current [`AvailabilityIndex`] snapshot to
    /// `_index/availability.bin` via an `Atomic` tempfile swap. Called
    /// from [`crate::AssetStore::checkpoint`]. No [`Drop`] hook is
    /// used — checkpointing is always explicit (closes attempt #1's
    /// landmine L3).
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the index resource cannot be opened
    /// or the atomic write fails.
    pub(crate) fn checkpoint(&self) -> AssetsResult<()> {
        let res = self.open_availability_index_resource()?;
        let atomic = Atomic::new(res);
        self.availability.persist_to(&atomic)
    }

    /// Spawn the background flusher that persists `_index/availability.bin`
    /// on two triggers:
    ///
    /// - every `checkpoint_every` commits observed by this store
    ///   (coalesced through [`CheckpointSignal`]);
    /// - when the owning cancel token fires (cooperative shutdown).
    ///
    /// No-op when `checkpoint_every` was not configured. Silently skipped
    /// when no tokio runtime is attached to the current thread — the
    /// aggregate then persists only via explicit
    /// [`crate::AssetStore::checkpoint`] calls (historical behaviour).
    pub(crate) fn spawn_auto_flush(store: Arc<Self>) {
        let Some(signal) = store.checkpoint_signal.clone() else {
            return;
        };
        let Ok(handle) = Handle::try_current() else {
            tracing::debug!(
                asset_root = %store.asset_root,
                "DiskAssetStore::spawn_auto_flush: no tokio runtime; auto-checkpoint disabled",
            );
            return;
        };

        let cancel = store.cancel.clone();
        handle.spawn(async move {
            loop {
                tokio::select! {
                    () = signal.notify.notified() => {
                        if let Err(e) = store.checkpoint() {
                            tracing::warn!(
                                asset_root = %store.asset_root,
                                error = %e,
                                "auto-checkpoint: flush failed",
                            );
                        }
                    }
                    () = cancel.cancelled() => {
                        if let Err(e) = store.checkpoint() {
                            tracing::warn!(
                                asset_root = %store.asset_root,
                                error = %e,
                                "auto-checkpoint: shutdown flush failed",
                            );
                        }
                        break;
                    }
                }
            }
        });
    }

    fn scoped_observer(&self, key: &ResourceKey) -> Arc<dyn AvailabilityObserver> {
        crate::index::ScopedAvailabilityObserver::new(
            self.asset_root.clone(),
            key.clone(),
            self.availability.clone(),
            self.checkpoint_signal.clone(),
        )
    }

    fn open_storage_resource(
        &self,
        key: &ResourceKey,
        path: PathBuf,
        mode: OpenMode,
    ) -> AssetsResult<MmapResource> {
        let resource = Resource::open_with_observer(
            self.cancel.clone(),
            MmapOptions {
                path,
                initial_len: None,
                mode,
            },
            Some(self.scoped_observer(key)),
        )?;
        // Seed aggregate from the driver's initial state for files
        // already committed on disk. `MmapDriver::open` populates
        // `available = 0..file_len` for existing files, so a caller
        // that never goes through `write_at` / `commit` (e.g. a
        // pre-existing packaged fixture) would otherwise be invisible
        // to `AssetStore::contains_range`. Closes landmine L2 for
        // everything that flows through this open path.
        if let ResourceStatus::Committed {
            final_len: Some(len),
        } = resource.status()
        {
            self.availability.record_commit(&self.asset_root, key, len);
        }
        Ok(resource)
    }

    fn open_index_resource(&self, path: PathBuf) -> AssetsResult<MmapResource> {
        Ok(Resource::open(
            self.cancel.clone(),
            MmapOptions {
                path,
                initial_len: Some(INDEX_INITIAL_SIZE),
                mode: OpenMode::ReadWrite,
            },
        )?)
    }
}

impl Assets for DiskAssetStore {
    type Res = StorageResource;
    type Context = ();
    type IndexRes = MmapResource;

    fn capabilities(&self) -> Capabilities {
        if self.asset_root.is_empty() {
            // Local-file mode: absolute keys only, no decorators.
            Capabilities::PROCESSING
        } else {
            Capabilities::all()
        }
    }

    fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        let path = self.resource_path(key)?;
        if !path.exists() {
            return Err(IoError::new(ErrorKind::NotFound, "resource missing").into());
        }
        let mmap = self.open_storage_resource(key, path, OpenMode::ReadOnly)?;
        Ok(StorageResource::Mmap(mmap))
    }

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        let path = self.resource_path(key)?;
        let mode = if key.is_absolute() {
            OpenMode::ReadOnly
        } else {
            OpenMode::Auto
        };
        let mmap = self.open_storage_resource(key, path, mode)?;
        Ok(StorageResource::Mmap(mmap))
    }

    fn open_pins_index_resource(&self) -> AssetsResult<MmapResource> {
        let path = self.pins_index_path();
        self.open_index_resource(path)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<MmapResource> {
        let path = self.lru_index_path();
        self.open_index_resource(path)
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        let path = self.resource_path(key)?;
        match fs::metadata(path) {
            Ok(metadata) => Ok(AssetResourceState::Committed {
                final_len: Some(metadata.len()),
            }),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(AssetResourceState::Missing),
            Err(error) => Err(error.into()),
        }
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(StorageError::Cancelled.into());
        }
        delete_asset_dir(&self.root_dir, &self.asset_root).map_err(Into::into)
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        let path = self.resource_path(key)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Delete an asset directory by `asset_root` directly via filesystem.
pub(crate) fn delete_asset_dir(root_dir: &Path, asset_root: &str) -> io::Result<()> {
    let safe = sanitize_rel(asset_root)
        .map_err(|()| IoError::new(ErrorKind::InvalidInput, "invalid asset_root"))?;
    let path = root_dir.join(safe);
    match fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub(crate) fn sanitize_rel(input: &str) -> Result<String, ()> {
    let s = input.replace('\\', "/");
    if s.is_empty() || s.starts_with('/') || s.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(());
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use kithara_storage::{ResourceExt, ResourceStatus};
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[kithara::test]
    #[case("valid.txt", true, "Simple filename")]
    #[case("dir/valid.txt", true, "Nested path")]
    #[case("a/b/c/file.mp3", true, "Multiple levels")]
    #[case("audio-file_123.m4a", true, "Filename with special chars")]
    #[case("/absolute", false, "Absolute path (leading slash)")]
    #[case("../traversal", false, "Dotdot traversal at start")]
    #[case("dir/../file", false, "Dotdot traversal in middle")]
    #[case("a/b/../c", false, "Dotdot traversal")]
    #[case("", false, "Empty string")]
    #[case("dir//file", false, "Double slash (empty component)")]
    #[case("dir/", false, "Trailing slash (empty component)")]
    #[case("/", false, "Single slash")]
    #[case(".", true, "Current directory reference")]
    #[case("dir/./file.txt", true, "Dot component (allowed)")]
    #[case("windows\\path", true, "Windows backslash (gets normalized)")]
    #[case("dir\\file.txt", true, "Mixed slashes")]
    fn test_path_validation(
        #[case] path: &str,
        #[case] is_valid: bool,
        #[case] _description: &str,
    ) {
        let result = sanitize_rel(path);
        assert_eq!(result.is_ok(), is_valid, "Path: {:?}", path);

        if is_valid {
            let normalized = result.unwrap();
            assert!(
                !normalized.contains('\\'),
                "Backslashes should be normalized"
            );
        }
    }

    #[kithara::test]
    fn test_open_absolute_resource_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("local_audio.mp3");
        fs::write(&file_path, b"fake audio data").unwrap();

        let store = DiskAssetStore::new(dir.path().join("cache"), "_", CancellationToken::new());

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key).unwrap();

        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        let mut buf = [0u8; 15];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"fake audio data");
    }
}
