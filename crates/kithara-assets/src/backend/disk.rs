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
    index::{ABSOLUTE_ROOT, AvailabilityIndex, PinDurability},
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
    /// Bytes a fresh segment's temp file is sized to on open. A segment
    /// arrives in many small chunks, and every time the writes outgrow the
    /// mapping the driver re-maps the file — the single most expensive step of
    /// a segment commit. One reservation that covers a typical segment removes
    /// those re-maps; anything larger still grows, and the surplus is trimmed
    /// back to `final_len` on commit, so this costs a sparse extent and
    /// nothing else.
    segment_reservation: u64,
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
    pub(crate) const fn new(
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
        let pins_result = self
            .pins
            .remove(asset_root, PinDurability::Durable)
            .map(|_| ());
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

#[bon::bon]
impl DiskAssetStore {
    /// Create a store rooted at `root_dir` with its own unshared
    /// [`AvailabilityIndex`]. Convenient for tests; production
    /// construction (via `AssetStore::builder().build()`) uses
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
        Self::with_availability_and_deleter()
            .root_dir(root_dir)
            .cancel(cancel)
            .availability(availability)
            .deleter(deleter)
            .call()
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

    fn open_absolute_resource(&self, key: &ResourceKey) -> AssetsResult<Option<BaseReader>> {
        let Some(path) = key.as_absolute_path() else {
            return Ok(None);
        };
        Ok(Some(BaseReader::open_read_only_file(path, &self.cancel)?))
    }

    /// Open a fresh segment as an `AtomicChunked<MmapResource>`. The
    /// inner mmap is bound to `<path>.tmp`; on `commit()` the tmp file is
    /// renamed atomically to `path`, and the durability barrier is deferred
    /// to the manifest flush rather than paid on the resource-write path. The
    /// availability observer is attached to the inner mmap so
    /// `record_write` / `record_commit` fire as bytes arrive — same
    /// contract as the non-atomic path.
    fn open_atomic_chunked_resource(
        &self,
        key: &ResourceKey,
        path: PathBuf,
    ) -> AssetsResult<AtomicChunked<MmapDriver>> {
        let observer = self.segment_observer(key, path.clone());
        let cancel = self.cancel.clone();
        let reservation = self.segment_reservation;
        let chunked = AtomicChunked::open_deferred(path, move |target, intent| {
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open_with_observer(
                cancel.clone(),
                MmapOptions::for_path(target.to_path_buf())
                    .mode(mode)
                    .maybe_initial_len((intent == OpenIntent::Fresh).then_some(reservation))
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

    /// Whether `key`'s bytes are known durable. The file existing is not
    /// enough: it becomes visible at `rename`, while the barrier that puts
    /// its blocks on the medium lands later, so an unconfirmed file may be
    /// the right length over unwritten blocks. The availability manifest is
    /// written only after that barrier, and is therefore the authority.
    fn is_confirmed(&self, key: &ResourceKey, path: &Path) -> bool {
        self.availability.final_len(key).is_some()
            && path.metadata().is_ok_and(|meta| meta.len() > 0)
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

    /// Observer for a segment: its commit hands the file to the manifest's
    /// durability barrier instead of paying one inline.
    fn segment_observer(&self, key: &ResourceKey, path: PathBuf) -> Arc<dyn AvailabilityObserver> {
        crate::index::ScopedAvailabilityObserver::for_file(
            key.clone(),
            self.availability.clone(),
            path,
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
    /// goes through it, see [`crate::backend::AssetDeleter`]. Production callers
    /// share one [`Arc<dyn AssetDeleter>`] between the store and the
    /// LRU evictor; tests construct a fresh deleter via
    /// [`Self::new`].
    /// `segment_reservation` defaults to one mebibyte, which covers a typical
    /// media segment in a single mapping.
    #[builder]
    pub(crate) fn with_availability_and_deleter<P: Into<PathBuf>>(
        root_dir: P,
        cancel: CancelToken,
        availability: AvailabilityIndex,
        deleter: Arc<dyn AssetDeleter>,
        #[builder(default = 1024 * 1024)] segment_reservation: u64,
    ) -> Self {
        Self {
            cancel,
            availability,
            deleter,
            root_dir: root_dir.into(),
            segment_reservation,
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
        if self.is_confirmed(key, &path) {
            let storage =
                StorageResource::from(self.open_storage_resource(key, path, OpenMode::Auto)?);
            if matches!(storage.status(), ResourceStatus::Committed { .. }) {
                return Ok(AcquisitionResult::Ready(BaseReader::new(storage)));
            }
            return Ok(AcquisitionResult::Pending(BaseWriter::new(storage)));
        }
        // Unconfirmed leftovers are indistinguishable from a torn write, so
        // they are refetched rather than trusted. Clear the path so the fresh
        // acquisition can claim its temp file.
        if path.exists() {
            let _ = fs::remove_file(&path);
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

/// The file an availability-index entry names, or `None` when the pair cannot
/// name one. The index files a resource under the same `(root, path)` split
/// [`DiskAssetStore::resource_path`] resolves, so both answer for one layout.
pub(crate) fn indexed_path(root_dir: &Path, root: &str, path: &str) -> Option<PathBuf> {
    if root == ABSOLUTE_ROOT {
        return Some(PathBuf::from(path));
    }
    Some(
        root_dir
            .join(sanitize_rel(root).ok()?)
            .join(sanitize_rel(path).ok()?),
    )
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
        resource::{ReadSide, WriteSide},
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

    /// Bytes reach the disk before the barrier that makes them durable, so a
    /// file on its own proves nothing: after a power cut it can carry the
    /// right name and length over unwritten blocks. Only the availability
    /// manifest, written after that barrier, says a resource is readable.
    #[kithara::test]
    fn an_unconfirmed_file_is_not_served_as_ready() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskAssetStore::new(
            dir.path().to_path_buf(),
            CancelToken::never(),
            &crate::BytePool::default(),
        );
        let key = ResourceKey::relative("asset", "segments/0001.bin");
        let path = dir.path().join("asset").join("segments").join("0001.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![0xEEu8; 4096]).unwrap();

        let acquired = store.acquire_resource(&key, None).unwrap();

        assert!(
            !acquired.is_ready(),
            "a file the manifest never confirmed must not be handed out as committed"
        );
    }

    /// A media segment arrives in many small chunks. If the backing file
    /// starts small and doubles, every crossing re-maps it — the dominant
    /// Build a store whose fresh segments reserve `reservation` bytes,
    /// bypassing the [`DiskAssetStore::new`] default.
    fn store_reserving(root: &Path, reservation: u64) -> DiskAssetStore {
        let root = root.to_path_buf();
        let availability = AvailabilityIndex::new();
        let deleter: Arc<dyn AssetDeleter> = Arc::new(DiskAssetDeleter::new(
            root.clone(),
            availability.clone(),
            PinsIndex::ephemeral(),
            LruIndex::ephemeral(),
        ));
        DiskAssetStore::with_availability_and_deleter()
            .root_dir(root)
            .cancel(CancelToken::never())
            .availability(availability)
            .deleter(deleter)
            .segment_reservation(reservation)
            .call()
    }

    fn segment_tmp_len(store: &DiskAssetStore, root: &Path) -> u64 {
        let key = ResourceKey::relative("asset", "segments/0001.bin");
        let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
            panic!("a fresh segment key must acquire a writer");
        };
        let len = fs::metadata(root.join("asset").join("segments").join("0001.bin.tmp"))
            .unwrap()
            .len();
        drop(writer);
        len
    }

    /// The reservation is the caller's, not a crate constant: a store built
    /// with a small one sizes its fresh temp file to exactly that.
    #[kithara::test]
    fn a_fresh_segment_reserves_the_size_the_store_was_built_with() {
        const RESERVATION: u64 = 8 * 1024;
        let dir = tempfile::tempdir().unwrap();

        let reserved = segment_tmp_len(&store_reserving(dir.path(), RESERVATION), dir.path());

        assert_eq!(reserved, RESERVATION);
    }

    #[kithara::test]
    fn a_larger_reservation_reaches_the_temp_file_too() {
        const RESERVATION: u64 = 512 * 1024;
        let dir = tempfile::tempdir().unwrap();

        let reserved = segment_tmp_len(&store_reserving(dir.path(), RESERVATION), dir.path());

        assert_eq!(reserved, RESERVATION);
    }

    /// cost of a segment commit. Writing a typical segment must leave the
    /// file's size untouched.
    #[kithara::test]
    fn writing_a_segment_does_not_resize_its_backing_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskAssetStore::new(
            dir.path().to_path_buf(),
            CancelToken::never(),
            &crate::BytePool::default(),
        );
        let key = ResourceKey::relative("asset", "segments/0001.bin");
        let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
            panic!("a fresh segment key must acquire a writer");
        };
        let tmp = dir
            .path()
            .join("asset")
            .join("segments")
            .join("0001.bin.tmp");
        let reserved = fs::metadata(&tmp).unwrap().len();

        let chunk = vec![0xABu8; 16 * 1024];
        for i in 0..12u64 {
            writer.write_at(i * chunk.len() as u64, &chunk).unwrap();
        }

        assert_eq!(
            fs::metadata(&tmp).unwrap().len(),
            reserved,
            "a segment-sized write must fit the reservation instead of re-mapping the file"
        );
    }

    #[kithara::test]
    fn a_committed_segment_keeps_only_its_own_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskAssetStore::new(
            dir.path().to_path_buf(),
            CancelToken::never(),
            &crate::BytePool::default(),
        );
        let key = ResourceKey::relative("asset", "segments/0001.bin");
        let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
            panic!("a fresh segment key must acquire a writer");
        };
        let payload = vec![0xCDu8; 100_000];
        writer.write_at(0, &payload).unwrap();

        drop(writer.commit(Some(payload.len() as u64)).unwrap());

        let canonical = dir.path().join("asset").join("segments").join("0001.bin");
        assert_eq!(
            fs::metadata(&canonical).unwrap().len(),
            payload.len() as u64,
            "the reservation must be trimmed back to the committed length"
        );
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
        pins.add(asset_root, PinDurability::Durable).unwrap();
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
