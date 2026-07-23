#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use std::{
    fs,
    io::{self, Error as IoError, ErrorKind},
    path::{Path, PathBuf},
};

use kithara_platform::{CancelToken, sync::Arc};
use kithara_storage::{
    AtomicChunked, AvailabilityObserver, MmapDriver, MmapOptions, MmapResource, OpenIntent,
    OpenMode, Resource, ResourceRead, ResourceStatus, StorageError, StorageResource,
};

use super::AssetDeleter;
use crate::{
    decorator::{Assets, Capabilities},
    error::{AssetsError, AssetsResult},
    index::AvailabilityIndex,
    layout::{ResourceKey, ResourceKeyKind},
    resource::{AcquisitionResult, AssetResourceState, BaseReader, BaseWriter, RequestIdentity},
};

/// Concrete on-disk [`Assets`] implementation.
///
/// One `DiskAssetStore` services every asset under its `root_dir`;
/// `asset_root` is a per-call parameter.
#[derive(Clone, Debug)]
pub struct DiskAssetStore {
    /// Single canonical removal channel. Synchronises FS deletion with
    /// the [`AvailabilityIndex`]. See [`AssetDeleter`] for the contract.
    deleter: Arc<dyn AssetDeleter>,
    availability: AvailabilityIndex,
    cancel: CancelToken,
    root_dir: PathBuf,
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
/// [`AssetDeleter`] for normative wording.
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
        delete_asset_dir(&self.root_dir, asset_root).map_err(AssetsError::from)?;
        self.availability.clear_root(asset_root);
        let pins_result = self.pins.remove(asset_root).map(|_| ());
        let lru_result = self.lru.remove(asset_root);
        pins_result.and(lru_result)
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        if key.is_absolute() {
            return Err(AssetsError::InvalidKey);
        }
        let path = match key.kind() {
            ResourceKeyKind::Relative {
                asset_root,
                rel_path,
            } => {
                let safe_root = sanitize_rel(asset_root).map_err(|()| AssetsError::InvalidKey)?;
                let safe_rel = sanitize_rel(rel_path).map_err(|()| AssetsError::InvalidKey)?;
                self.root_dir.join(safe_root).join(safe_rel)
            }
            ResourceKeyKind::Absolute(_) => return Err(AssetsError::InvalidKey),
        };
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.availability.remove(key);
        Ok(())
    }
}

impl DiskAssetStore {
    /// Create a store rooted at `root_dir` with its own unshared
    /// [`AvailabilityIndex`]. Convenient for tests; production
    /// construction (via `AssetStoreBuilder::build`) uses
    /// [`DiskAssetStore::with_availability_and_deleter`].
    pub fn new<P: Into<PathBuf>>(
        root_dir: P,
        cancel: CancelToken,
        _pool: &kithara_bufpool::BytePool,
    ) -> Self {
        let root_dir = root_dir.into();
        let availability = AvailabilityIndex::new();
        let pins = crate::index::PinsIndex::ephemeral();
        let lru = crate::index::LruIndex::ephemeral();
        let deleter: Arc<dyn AssetDeleter> = Arc::new(DiskAssetDeleter::new(
            root_dir.clone(),
            availability.clone(),
            pins,
            lru,
        ));
        Self::with_availability_and_deleter(root_dir, cancel, availability, deleter)
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
    ) -> AssetsResult<AtomicChunked<MmapDriver>> {
        let observer = self.scoped_observer(key);
        let cancel = self.cancel.clone();
        let chunked = AtomicChunked::open(path, move |target, intent| {
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open_with_observer(
                cancel.clone(),
                MmapOptions::for_path(target.to_path_buf())
                    .mode(mode)
                    .build(),
                Some(Arc::clone(&observer) as Arc<dyn AvailabilityObserver>),
            )
        })?;
        Ok(chunked)
    }

    fn open_index_resource(&self, path: PathBuf) -> AssetsResult<MmapResource> {
        /// Initial mmap file size for index resources (4 KB).
        const INDEX_INITIAL_SIZE: u64 = 4096;
        Ok(Resource::open(
            self.cancel.clone(),
            MmapOptions::for_path(path)
                .initial_len(INDEX_INITIAL_SIZE)
                .mode(OpenMode::ReadWrite)
                .build(),
        )?)
    }

    fn open_absolute_resource(&self, key: &ResourceKey) -> AssetsResult<Option<BaseReader>> {
        let Some(path) = key.as_absolute_path() else {
            return Ok(None);
        };
        Ok(Some(BaseReader::open_read_only_file(path, &self.cancel)?))
    }

    fn open_storage_resource(
        &self,
        key: &ResourceKey,
        path: PathBuf,
        mode: OpenMode,
    ) -> AssetsResult<MmapResource> {
        let resource = Resource::open_with_observer(
            self.cancel.clone(),
            MmapOptions::for_path(path).mode(mode).build(),
            Some(self.scoped_observer(key)),
        )?;
        if let ResourceStatus::Committed {
            final_len: Some(len),
        } = resource.status()
        {
            self.availability.record_commit(key, len);
        }
        Ok(resource)
    }

    fn pins_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("pins.bin")
    }

    fn resource_path(&self, key: &ResourceKey) -> AssetsResult<PathBuf> {
        match key.kind() {
            ResourceKeyKind::Relative {
                asset_root,
                rel_path,
            } => {
                let asset_root_safe =
                    sanitize_rel(asset_root).map_err(|()| AssetsError::InvalidKey)?;
                let rel = sanitize_rel(rel_path).map_err(|()| AssetsError::InvalidKey)?;
                Ok(self.root_dir.join(asset_root_safe).join(rel))
            }
            ResourceKeyKind::Absolute(path) => Ok(path.clone()),
        }
    }

    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn scoped_observer(&self, key: &ResourceKey) -> Arc<dyn AvailabilityObserver> {
        crate::index::ScopedAvailabilityObserver::new(key.clone(), self.availability.clone())
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
    /// goes through it, see [`crate::backend::AssetDeleter`]. Production callers
    /// share one [`Arc<dyn AssetDeleter>`] between the store and the
    /// LRU evictor; tests construct a fresh deleter via
    /// [`Self::new`].
    pub(crate) fn with_availability_and_deleter<P: Into<PathBuf>>(
        root_dir: P,
        cancel: CancelToken,
        availability: AvailabilityIndex,
        deleter: Arc<dyn AssetDeleter>,
    ) -> Self {
        Self {
            cancel,
            availability,
            deleter,
            root_dir: root_dir.into(),
        }
    }
}

impl Assets for DiskAssetStore {
    type ActiveRes = BaseWriter;
    type Context = ();
    type IndexRes = StorageResource;
    type ReadyRes = BaseReader;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _identity: Option<&RequestIdentity>,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<BaseWriter, BaseReader>> {
        if let Some(reader) = self.open_absolute_resource(key)? {
            return Ok(AcquisitionResult::Ready(reader));
        }

        let path = self.resource_path(key)?;
        if path.exists() && path.metadata().is_ok_and(|m| m.len() > 0) {
            let storage =
                StorageResource::from(self.open_storage_resource(key, path, OpenMode::Auto)?);
            if matches!(storage.status(), ResourceStatus::Committed { .. }) {
                return Ok(AcquisitionResult::Ready(BaseReader::new(storage)));
            }
            return Ok(AcquisitionResult::Pending(BaseWriter::new(storage)));
        }
        let chunked = self.open_atomic_chunked_resource(key, path)?;
        Ok(AcquisitionResult::Pending(BaseWriter::new(
            StorageResource::from(chunked),
        )))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }

    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(StorageError::Cancelled.into());
        }
        self.deleter.delete_asset(asset_root)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<StorageResource> {
        let path = self.lru_index_path();
        Ok(StorageResource::from(self.open_index_resource(path)?))
    }

    fn open_pins_index_resource(&self) -> AssetsResult<StorageResource> {
        let path = self.pins_index_path();
        Ok(StorageResource::from(self.open_index_resource(path)?))
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _identity: Option<&RequestIdentity>,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<BaseReader> {
        if let Some(reader) = self.open_absolute_resource(key)? {
            return Ok(reader);
        }

        let path = self.resource_path(key)?;
        if !path.exists() {
            return Err(IoError::new(ErrorKind::NotFound, "resource missing").into());
        }
        let mmap = self.open_storage_resource(key, path, OpenMode::ReadOnly)?;
        Ok(BaseReader::new(StorageResource::from(mmap)))
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.deleter.remove_resource(key)
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
    use std::collections::HashSet;

    use kithara_platform::CancelToken;
    use kithara_storage::ResourceStatus;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        index::{EvictConfig, LruIndex, PinsIndex},
        resource::ReadSide,
    };

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
            CancelToken::never(),
            &crate::BytePool::default(),
        );

        let key = ResourceKey::absolute(&file_path).expect("absolute test path");
        let res = store.open_resource(&key, None).unwrap();

        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        let mut buf = [0u8; 15];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"fake audio data");
    }

    #[kithara::test]
    fn direct_store_cannot_remove_absolute_resource() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("local_audio.mp3");
        fs::write(&file_path, b"fake audio data").unwrap();
        let store = DiskAssetStore::new(
            dir.path().join("cache"),
            CancelToken::never(),
            &crate::BytePool::default(),
        );
        let key = ResourceKey::absolute(&file_path).expect("absolute test path");

        assert!(matches!(
            store.remove_resource(&key),
            Err(AssetsError::InvalidKey)
        ));
        assert_eq!(fs::read(file_path).unwrap(), b"fake audio data");
    }

    #[kithara::test]
    fn failed_asset_delete_keeps_all_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let asset_root = "undeletable-asset";
        let asset_path = dir.path().join(asset_root);
        fs::write(&asset_path, b"not a directory").unwrap();

        let availability = AvailabilityIndex::new();
        let key = ResourceKey::relative(asset_root, "track.bin");
        availability.record_commit(&key, 4);

        let pins = PinsIndex::ephemeral();
        pins.add(asset_root).unwrap();
        let lru = LruIndex::ephemeral();
        lru.touch(asset_root, Some(4)).unwrap();

        let deleter = DiskAssetDeleter::new(
            dir.path().into(),
            availability.clone(),
            pins.clone(),
            lru.clone(),
        );

        assert!(matches!(
            deleter.delete_asset(asset_root),
            Err(AssetsError::Io(_))
        ));
        assert!(asset_path.is_file());
        assert!(availability.contains_range(&key, 0..4));
        assert!(pins.contains(asset_root));
        assert_eq!(
            lru.eviction_candidates(
                &EvictConfig {
                    max_assets: Some(0),
                    max_bytes: None,
                },
                &HashSet::new(),
            ),
            vec![asset_root.to_string()]
        );
    }

    #[kithara::test]
    fn failed_resource_delete_keeps_availability() {
        let dir = tempfile::tempdir().unwrap();
        let asset_root = "asset";
        let rel_path = "track.bin";
        let resource_path = dir.path().join(asset_root).join(rel_path);
        fs::create_dir_all(&resource_path).unwrap();

        let availability = AvailabilityIndex::new();
        let key = ResourceKey::relative(asset_root, rel_path);
        availability.record_commit(&key, 4);
        let deleter = DiskAssetDeleter::new(
            dir.path().into(),
            availability.clone(),
            PinsIndex::ephemeral(),
            LruIndex::ephemeral(),
        );

        assert!(matches!(
            deleter.remove_resource(&key),
            Err(AssetsError::Io(_))
        ));
        assert!(resource_path.is_dir());
        assert!(availability.contains_range(&key, 0..4));
    }
}
