#![forbid(unsafe_code)]

use std::{
    collections::HashSet,
    fmt::Debug,
    ops::Range,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_storage::{ResourceExt, ResourceStatus, StorageResource, StorageResult, WaitOutcome};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    base::Assets, error::AssetsResult, evict::ByteRecorder, index::PinsIndex, key::ResourceKey,
};

/// Decorator that adds "pin (lease) while handle lives" semantics on top of inner [`Assets`].
///
/// ## Normative behavior
/// - Every successful `open_resource()` pins `key.asset_root`.
/// - The pin table is stored as a `HashSet<String>` (unique roots) in memory.
/// - Pin/unpin operations set a dirty flag; disk persistence is deferred until
///   [`flush_pins()`](Self::flush_pins) is called or the last clone is dropped.
/// - The pin index resource must be excluded from pinning to avoid recursion.
///
/// This type does **not** do any filesystem/path logic; it uses the inner `Assets` abstraction.
#[derive(Clone)]
pub struct LeaseAssets<A>
where
    A: Assets,
{
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    cancel: CancellationToken,
    dirty: Arc<AtomicBool>,
    inner: Arc<A>,
    pins: Arc<Mutex<HashSet<String>>>,
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    pub fn new(inner: Arc<A>, cancel: CancellationToken) -> Self {
        Self {
            byte_recorder: None,
            cancel,
            dirty: Arc::new(AtomicBool::new(false)),
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Create with byte recorder for eviction tracking.
    pub fn with_byte_recorder(
        inner: Arc<A>,
        cancel: CancellationToken,
        byte_recorder: Arc<dyn ByteRecorder>,
    ) -> Self {
        Self {
            byte_recorder: Some(byte_recorder),
            cancel,
            dirty: Arc::new(AtomicBool::new(false)),
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub(crate) fn inner(&self) -> &A {
        &self.inner
    }

    fn open_index(&self) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.inner())
    }

    fn persist_pins_best_effort(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        let idx = self.open_index()?;
        idx.store(pins)
    }

    fn load_pins_best_effort(&self) -> AssetsResult<HashSet<String>> {
        let idx = self.open_index()?;
        idx.load()
    }

    fn ensure_loaded_best_effort(&self) -> AssetsResult<()> {
        let should_load = { self.pins.lock().is_empty() };

        if !should_load {
            return Ok(());
        }

        let loaded = self.load_pins_best_effort()?;
        let mut guard = self.pins.lock();
        for p in loaded {
            guard.insert(p);
        }
        Ok(())
    }

    /// Persist the current pin set to disk if dirty, then clear the dirty flag.
    pub fn flush_pins(&self) -> AssetsResult<()> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let snapshot = self.pins.lock().clone();
        self.persist_pins_best_effort(&snapshot)
    }

    fn pin(&self, asset_root: &str) -> AssetsResult<LeaseGuard<A>> {
        self.ensure_loaded_best_effort()?;

        {
            let mut pins = self.pins.lock();
            if pins.insert(asset_root.to_string()) {
                self.dirty.store(true, Ordering::Release);
            }
        }

        Ok(LeaseGuard {
            inner: Arc::new(LeaseGuardInner {
                owner: self.clone(),
                asset_root: asset_root.to_string(),
                cancel: self.cancel.clone(),
            }),
        })
    }
}

/// Resource wrapper that combines lease guard with byte recording on commit.
#[derive(Clone)]
pub struct LeaseResource<R, L> {
    inner: R,
    _lease: L,
    asset_root: String,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
}

impl<R, L> Debug for LeaseResource<R, L>
where
    R: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseResource")
            .field("inner", &self.inner)
            .field("asset_root", &self.asset_root)
            .finish_non_exhaustive()
    }
}

impl<R, L> ResourceExt for LeaseResource<R, L>
where
    R: ResourceExt + Send + Sync + Clone + Debug + 'static,
    L: Send + Sync + Clone + 'static,
{
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.inner.wait_range(range)
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.inner.commit(final_len)?;

        // Record bytes if recorder is set (best-effort)
        if let Some(ref recorder) = self.byte_recorder
            && let Ok(metadata) = std::fs::metadata(self.inner.path())
            && metadata.is_file()
        {
            recorder.record_bytes(&self.asset_root, metadata.len());
        }

        Ok(())
    }

    fn fail(&self, reason: String) {
        self.inner.fail(reason);
    }

    fn path(&self) -> &Path {
        self.inner.path()
    }

    fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.inner.reactivate()
    }

    fn status(&self) -> ResourceStatus {
        self.inner.status()
    }
}

impl<A> Assets for LeaseAssets<A>
where
    A: Assets,
{
    type Res = LeaseResource<A::Res, LeaseGuard<A>>;
    type Context = A::Context;

    fn root_dir(&self) -> &Path {
        self.inner.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.inner.asset_root()
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        let inner = self.inner.open_resource_with_ctx(key, ctx)?;
        let lease = self.pin(self.inner.asset_root())?;

        Ok(LeaseResource {
            inner,
            _lease: lease,
            asset_root: self.inner.asset_root().to_string(),
            byte_recorder: self.byte_recorder.clone(),
        })
    }

    fn open_pins_index_resource(&self) -> AssetsResult<StorageResource> {
        // Pins index must be opened without pinning to avoid recursion
        self.inner.open_pins_index_resource()
    }

    fn open_lru_index_resource(&self) -> AssetsResult<StorageResource> {
        // LRU index must be opened without pinning
        self.inner.open_lru_index_resource()
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        self.inner.delete_asset()
    }
}

impl<A> Drop for LeaseAssets<A>
where
    A: Assets,
{
    fn drop(&mut self) {
        // Only persist on the last clone (all Arc fields share the same refcount via `pins`)
        if Arc::strong_count(&self.pins) == 1 && self.dirty.load(Ordering::Acquire) {
            let snapshot = self.pins.lock().clone();
            let _ = self.persist_pins_best_effort(&snapshot);
        }
    }
}

/// RAII guard for a pin.
///
/// Dropping this guard unpins the corresponding `asset_root` and persists the new pin set
/// to disk (best-effort) via the decorator.
///
/// Uses `Arc` internally for reference counting - unpin happens only when the last clone is dropped.
#[derive(Clone)]
pub struct LeaseGuard<A>
where
    A: Assets,
{
    #[expect(dead_code)]
    inner: Arc<LeaseGuardInner<A>>,
}

struct LeaseGuardInner<A>
where
    A: Assets,
{
    owner: LeaseAssets<A>,
    asset_root: String,
    cancel: CancellationToken,
}

impl<A> Drop for LeaseGuardInner<A>
where
    A: Assets,
{
    fn drop(&mut self) {
        if self.cancel.is_cancelled() {
            return;
        }

        tracing::trace!(asset_root = %self.asset_root, "LeaseGuard::drop - removing pin");

        let mut pins = self.owner.pins.lock();
        if pins.remove(&self.asset_root) {
            self.owner.dirty.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;

    use super::*;
    use crate::{base::DiskAssetStore, index::PinsIndex, key::ResourceKey};

    fn make_lease(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
        ));
        LeaseAssets::new(disk, CancellationToken::new())
    }

    fn load_persisted_pins(dir: &Path) -> HashSet<String> {
        let disk = DiskAssetStore::new(dir, "test_asset", CancellationToken::new());
        match PinsIndex::open(&disk) {
            Ok(idx) => idx.load().unwrap_or_default(),
            Err(_) => HashSet::new(),
        }
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn pin_does_not_persist_until_flush() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::new("audio.mp3");

        // Open a resource — this pins `test_asset`
        let _res = lease.open_resource(&key).unwrap();

        // Pins should NOT be on disk yet
        let on_disk = load_persisted_pins(dir.path());
        assert!(on_disk.is_empty(), "pins should not be persisted yet");

        // After flush, pins should be on disk
        lease.flush_pins().unwrap();
        let on_disk = load_persisted_pins(dir.path());
        assert!(
            on_disk.contains("test_asset"),
            "pins should be persisted after flush"
        );
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn flush_clears_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::new("audio.mp3");

        let _res = lease.open_resource(&key).unwrap();
        assert!(lease.dirty.load(Ordering::Acquire));

        lease.flush_pins().unwrap();
        assert!(!lease.dirty.load(Ordering::Acquire));

        // Second flush is a no-op
        lease.flush_pins().unwrap();
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn drop_last_lease_persists_dirty_pins() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::new("audio.mp3");

        let lease = make_lease(dir.path());
        let res = lease.open_resource(&key).unwrap();

        // Not persisted yet
        assert!(load_persisted_pins(dir.path()).is_empty());

        // Drop guard first (unpins test_asset, sets dirty)
        drop(res);

        // Drop the last lease — should persist via Drop
        drop(lease);

        // Verify the Drop path ran (persisted the empty pin set after unpin).
        // The file exists because persist was called — this is the key assertion.
        let disk = DiskAssetStore::new(dir.path(), "test_asset", CancellationToken::new());
        assert!(PinsIndex::open(&disk).is_ok());
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn flush_persists_active_pins() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::new("audio.mp3");

        let lease = make_lease(dir.path());
        let _res = lease.open_resource(&key).unwrap();

        // While resource is still held, flush should persist the active pin
        lease.flush_pins().unwrap();
        let on_disk = load_persisted_pins(dir.path());
        assert!(
            on_disk.contains("test_asset"),
            "flush should persist active pins"
        );
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn drop_intermediate_clone_does_not_persist() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::new("audio.mp3");

        let lease = make_lease(dir.path());
        let lease_clone = lease.clone();
        let _res = lease_clone.open_resource(&key).unwrap();

        // Drop clone (not the last — lease still exists, plus _res holds inner clone)
        drop(lease_clone);
        assert!(
            load_persisted_pins(dir.path()).is_empty(),
            "intermediate clone drop should not persist"
        );

        // Flush through remaining lease
        lease.flush_pins().unwrap();
        assert!(
            load_persisted_pins(dir.path()).contains("test_asset"),
            "flush on remaining clone should persist"
        );
    }
}
