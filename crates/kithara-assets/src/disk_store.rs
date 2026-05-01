#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use std::{
    fs,
    io::{self, Error as IoError, ErrorKind},
    path::{Path, PathBuf},
    sync::Arc,
};

use kithara_storage::{
    AtomicChunked, AvailabilityObserver, MmapOptions, MmapResource, OpenIntent, OpenMode, Resource,
    ResourceExt, ResourceStatus, StorageError, StorageResource,
};
use tokio_util::sync::CancellationToken;

use crate::{
    base::{Assets, Capabilities},
    deleter::AssetDeleter,
    error::{AssetsError, AssetsResult},
    index::AvailabilityIndex,
    key::ResourceKey,
    state::AssetResourceState,
};

/// Initial mmap file size for index resources (4 KB).
const INDEX_INITIAL_SIZE: u64 = 4096;

/// Concrete on-disk [`Assets`] implementation for a single asset.
///
/// Maps [`ResourceKey`] to disk paths under a root directory.
/// Each `DiskAssetStore` is scoped to a single `asset_root`.
#[derive(Clone, Debug)]
pub struct DiskAssetStore {
    /// Single canonical removal channel. Synchronises FS deletion with
    /// the [`AvailabilityIndex`]. See [`crate::deleter`] module docs.
    deleter: Arc<dyn AssetDeleter>,
    availability: AvailabilityIndex,
    cancel: CancellationToken,
    root_dir: PathBuf,
    asset_root: String,
}

/// Disk-backed [`AssetDeleter`].
///
/// Owns clones of every shared in-memory + disk-backed index handle
/// (`availability`, `pins`, `lru`) plus `root_dir`. `asset_root` is
/// **not** stored on the deleter itself — every method takes it as a
/// parameter so one deleter instance services own-asset teardown,
/// resource-level removal, and foreign-asset LRU eviction (the
/// call-site supplies the right name).
///
/// Contract: every method synchronises the FS-side change (or absence
/// thereof) with **all** indexes that reflect on-disk state — see
/// [`crate::deleter`] for normative wording.
#[derive(Debug)]
pub(crate) struct DiskAssetDeleter {
    availability: AvailabilityIndex,
    lru: crate::index::LruIndex,
    root_dir: PathBuf,
    pins: crate::index::PinsIndex,
}

impl DiskAssetDeleter {
    pub(crate) fn new(
        root_dir: PathBuf,
        availability: AvailabilityIndex,
        pins: crate::index::PinsIndex,
        lru: crate::index::LruIndex,
    ) -> Self {
        Self {
            availability,
            lru,
            root_dir,
            pins,
        }
    }
}

impl AssetDeleter for DiskAssetDeleter {
    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        // Asset-level removal: FS + every index that ties state to
        // this asset_root must be cleared. We attempt **all** of
        // them even on partial failure so the index that did manage
        // to update isn't left out of sync; the first error wins
        // for the return value.
        let fs_result = delete_asset_dir(&self.root_dir, asset_root).map_err(AssetsError::from);
        self.availability.clear_root(asset_root);
        let pins_result = self.pins.remove(asset_root).map(|_| ());
        let lru_result = self.lru.remove(asset_root);
        fs_result.and(pins_result).and(lru_result)
    }

    fn remove_resource(&self, asset_root: &str, key: &ResourceKey) -> AssetsResult<()> {
        let path = match key {
            ResourceKey::Relative(rel) => {
                let safe_root = sanitize_rel(asset_root).map_err(|()| AssetsError::InvalidKey)?;
                let safe_rel = sanitize_rel(rel).map_err(|()| AssetsError::InvalidKey)?;
                self.root_dir.join(safe_root).join(safe_rel)
            }
            ResourceKey::Absolute(path) => path.clone(),
        };
        let result = match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        };
        // Resource-level removal only invalidates the per-resource
        // entry in `AvailabilityIndex`. Pins/LRU are per-asset_root,
        // so they are untouched by a single-resource removal.
        self.availability.remove(asset_root, key);
        result
    }
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
        _pool: &kithara_bufpool::BytePool,
    ) -> Self {
        let root_dir = root_dir.into();
        let availability = AvailabilityIndex::new();
        // Standalone construction (tests, ad-hoc callers): ephemeral
        // indexes carry no on-disk state and need no pool. The
        // production builder uses disk-backed instances shared with
        // `LeaseAssets` and `EvictAssets`.
        let pins = crate::index::PinsIndex::ephemeral();
        let lru = crate::index::LruIndex::ephemeral();
        let deleter: Arc<dyn AssetDeleter> = Arc::new(DiskAssetDeleter::new(
            root_dir.clone(),
            availability.clone(),
            pins,
            lru,
        ));
        Self::with_availability_and_deleter(root_dir, asset_root, cancel, availability, deleter)
    }

    #[must_use]
    pub fn asset_root(&self) -> &str {
        &self.asset_root
    }

    /// Persist the current [`AvailabilityIndex`] snapshot to
    /// `_index/availability.bin`. Routes through the shared
    /// [`crate::index::FlushHub`] when one is attached (drains every
    /// dirty source — pins/lru/availability — under a single
    /// `flush_lock`); falls back to the inline serialise+write path
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the index resource cannot be opened
    /// or the atomic write fails.
    pub(crate) fn checkpoint(&self) -> AssetsResult<()> {
        self.availability.flush()
    }

    fn lru_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("lru.bin")
    }

    /// Open a fresh segment as an `AtomicChunked<MmapResource>`. The
    /// inner mmap is bound to `<path>.tmp`; on `commit()` the tmp
    /// file is `sync_data`'d and renamed atomically to `path`. The
    /// availability observer is attached to the inner mmap so
    /// `record_write` / `record_commit` fire as bytes arrive — same
    /// contract as the non-atomic path.
    fn open_atomic_chunked_resource(
        &self,
        key: &ResourceKey,
        path: PathBuf,
    ) -> AssetsResult<AtomicChunked<MmapResource>> {
        let observer = self.scoped_observer(key);
        let cancel = self.cancel.clone();
        // Factory is `Fn + Send + Sync + 'static`: called twice — once
        // here on the temp path, and again from `AtomicChunked::commit`
        // on the canonical path after the atomic rename — so the
        // closure captures by move and remains usable after.
        let chunked = AtomicChunked::open(path, move |target, intent| {
            // Fresh open at tmp path: ReadWrite so writers can fill
            // the segment. Reopen at canonical post-rename: ReadOnly
            // so the resource reports `Committed` status (otherwise
            // `LeaseResource::drop` would mistake it for an
            // abandoned writer and delete the just-renamed file).
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open_with_observer(
                cancel.clone(),
                MmapOptions {
                    mode,
                    path: target.to_path_buf(),
                    initial_len: None,
                },
                Some(Arc::clone(&observer) as Arc<dyn AvailabilityObserver>),
            )
        })?;
        Ok(chunked)
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
                mode,
                initial_len: None,
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

    fn pins_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("pins.bin")
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

    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn scoped_observer(&self, key: &ResourceKey) -> Arc<dyn AvailabilityObserver> {
        crate::index::ScopedAvailabilityObserver::new(
            self.asset_root.clone(),
            key.clone(),
            self.availability.clone(),
        )
    }

    /// Like [`DiskAssetStore::new`] but shares the given aggregate
    /// availability handle. Observer callbacks fired by this store's
    /// resources mutate the shared handle, so queries through the
    /// owning [`crate::AssetStore`] see the updates immediately.
    ///
    /// Disk persistence (load + later flush) is driven by
    /// [`AvailabilityIndex::enable_persistence`], which the production
    /// builder calls before constructing the store. Without it, the
    /// aggregate stays in-memory only.
    ///
    /// The `deleter` parameter is the canonical removal channel —
    /// every path that physically deletes a resource (own or foreign)
    /// goes through it, see [`crate::deleter`]. Production callers
    /// share one [`Arc<dyn AssetDeleter>`] between the store and the
    /// LRU evictor; tests construct a fresh deleter via
    /// [`Self::new`].
    pub(crate) fn with_availability_and_deleter<P: Into<PathBuf>, S: Into<String>>(
        root_dir: P,
        asset_root: S,
        cancel: CancellationToken,
        availability: AvailabilityIndex,
        deleter: Arc<dyn AssetDeleter>,
    ) -> Self {
        Self {
            cancel,
            availability,
            deleter,
            root_dir: root_dir.into(),
            asset_root: asset_root.into(),
        }
    }
}

impl Assets for DiskAssetStore {
    type Context = ();
    type IndexRes = MmapResource;
    type Res = StorageResource;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        let path = self.resource_path(key)?;
        // Absolute keys (local files) and re-opens of existing
        // canonical files: passthrough mode (read-only / already
        // durable).
        if key.is_absolute() || (path.exists() && path.metadata().is_ok_and(|m| m.len() > 0)) {
            let mode = if key.is_absolute() {
                OpenMode::ReadOnly
            } else {
                OpenMode::Auto
            };
            let mmap = self.open_storage_resource(key, path, mode)?;
            return Ok(mmap.into());
        }
        // Fresh segment write: wrap with `AtomicChunked` so writes
        // land at `<canonical>.tmp` and are atomic-renamed on
        // commit. Slow-path observers of the canonical path see
        // either no file or fully durable bytes — never partial.
        let chunked = self.open_atomic_chunked_resource(key, path)?;
        Ok(StorageResource::from(chunked))
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn capabilities(&self) -> Capabilities {
        if self.asset_root.is_empty() {
            // Local-file mode: absolute keys only, no decorators.
            Capabilities::PROCESSING
        } else {
            Capabilities::all()
        }
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(StorageError::Cancelled.into());
        }
        // Delegate to the canonical deleter — physical FS removal and
        // AvailabilityIndex invalidation happen atomically inside.
        // No other path is allowed to call `delete_asset_dir` or
        // `availability.clear_root` directly. See [`crate::deleter`].
        self.deleter.delete_asset(&self.asset_root)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<MmapResource> {
        let path = self.lru_index_path();
        self.open_index_resource(path)
    }

    fn open_pins_index_resource(&self) -> AssetsResult<MmapResource> {
        let path = self.pins_index_path();
        self.open_index_resource(path)
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
        // Re-open of an already-committed resource: passthrough mode
        // (no atomicity needed — bytes are durable on disk).
        let mmap = self.open_storage_resource(key, path, OpenMode::ReadOnly)?;
        Ok(mmap.into())
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        // Single canonical removal channel, see `delete_asset` above.
        self.deleter.remove_resource(&self.asset_root, key)
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

    fn root_dir(&self) -> &Path {
        &self.root_dir
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

        let store = DiskAssetStore::new(
            dir.path().join("cache"),
            "_",
            CancellationToken::new(),
            &crate::BytePool::default(),
        );

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key).unwrap();

        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        let mut buf = [0u8; 15];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"fake audio data");
    }
}
