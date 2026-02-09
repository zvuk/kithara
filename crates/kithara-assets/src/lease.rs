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
/// - Pin/unpin operations eagerly persist to disk (best-effort) so that
///   crash-recovery sees the current pin set immediately.
/// - The pin index resource must be excluded from pinning to avoid recursion.
/// - When `enabled` is `false`, all operations delegate directly to the inner layer
///   (no pinning, no byte recording, no persistence).
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
    enabled: bool,
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
            enabled: true,
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
            enabled: true,
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Create with explicit enabled flag. When `false`, lease/pin logic is bypassed.
    pub fn with_enabled(
        inner: Arc<A>,
        cancel: CancellationToken,
        byte_recorder: Option<Arc<dyn ByteRecorder>>,
        enabled: bool,
    ) -> Self {
        Self {
            byte_recorder,
            cancel,
            dirty: Arc::new(AtomicBool::new(false)),
            enabled,
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
        if !self.enabled || !self.dirty.swap(false, Ordering::AcqRel) {
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

        // Eagerly persist so crash-recovery sees the pin.
        let _ = self.flush_pins();

        Ok(LeaseGuard {
            inner: Some(Arc::new(LeaseGuardInner {
                owner: self.clone(),
                asset_root: asset_root.to_string(),
                cancel: self.cancel.clone(),
            })),
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
            && let Some(path) = self.inner.path()
            && let Ok(metadata) = std::fs::metadata(path)
            && metadata.is_file()
        {
            recorder.record_bytes(&self.asset_root, metadata.len());
        }

        Ok(())
    }

    fn fail(&self, reason: String) {
        self.inner.fail(reason);
    }

    fn path(&self) -> Option<&Path> {
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

        if !self.enabled {
            return Ok(LeaseResource {
                inner,
                _lease: LeaseGuard { inner: None },
                asset_root: self.inner.asset_root().to_string(),
                byte_recorder: None,
            });
        }

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
        if !self.enabled {
            return;
        }
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
/// When `inner` is `None`, the guard is a no-op (used when lease is bypassed).
#[derive(Clone)]
pub struct LeaseGuard<A>
where
    A: Assets,
{
    #[expect(dead_code)]
    inner: Option<Arc<LeaseGuardInner<A>>>,
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

        {
            let mut pins = self.owner.pins.lock();
            if pins.remove(&self.asset_root) {
                self.owner.dirty.store(true, Ordering::Release);
            }
        }

        // Eagerly persist unpin so crash-recovery sees it.
        let _ = self.owner.flush_pins();
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

    fn make_lease_disabled(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
        ));
        LeaseAssets::with_enabled(disk, CancellationToken::new(), None, false)
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
    fn pin_persists_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::new("audio.mp3");

        // Open a resource — this pins `test_asset`
        let _res = lease.open_resource(&key).unwrap();

        // Pins should be on disk immediately (eager persistence)
        let on_disk = load_persisted_pins(dir.path());
        assert!(
            on_disk.contains("test_asset"),
            "pin should be persisted immediately"
        );
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn eager_flush_clears_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::new("audio.mp3");

        let _res = lease.open_resource(&key).unwrap();

        // Dirty flag is already cleared by eager flush in pin()
        assert!(
            !lease.dirty.load(Ordering::Acquire),
            "dirty flag should be cleared by eager flush"
        );

        // Explicit flush is a no-op
        lease.flush_pins().unwrap();
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn drop_guard_eagerly_persists_unpin() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::new("audio.mp3");

        let lease = make_lease(dir.path());
        let res = lease.open_resource(&key).unwrap();

        // Pin is persisted eagerly
        assert!(load_persisted_pins(dir.path()).contains("test_asset"));

        // Drop guard (unpins test_asset, eagerly persists)
        drop(res);

        // After unpin, the pin set on disk should be empty
        let on_disk = load_persisted_pins(dir.path());
        assert!(
            on_disk.is_empty(),
            "unpin should be eagerly persisted, got {:?}",
            on_disk
        );
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
    fn clone_pin_persists_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::new("audio.mp3");

        let lease = make_lease(dir.path());
        let lease_clone = lease.clone();
        let _res = lease_clone.open_resource(&key).unwrap();

        // Pin should be persisted eagerly, even through a clone
        assert!(
            load_persisted_pins(dir.path()).contains("test_asset"),
            "pin via clone should be persisted immediately"
        );

        // Drop clone (not the last — lease still exists, plus _res holds inner clone)
        drop(lease_clone);

        // Pin should still be on disk (resource handle still alive)
        assert!(
            load_persisted_pins(dir.path()).contains("test_asset"),
            "pin should remain while resource handle is alive"
        );
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn bypass_does_not_pin() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease_disabled(dir.path());
        let key = ResourceKey::new("audio.mp3");

        let _res = lease.open_resource(&key).unwrap();

        // Pins should be empty when disabled
        let pins = lease.pins.lock();
        assert!(pins.is_empty(), "bypass should not pin");
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn bypass_flush_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease_disabled(dir.path());
        let key = ResourceKey::new("audio.mp3");

        let _res = lease.open_resource(&key).unwrap();
        lease.flush_pins().unwrap();

        // Nothing should be persisted
        let on_disk = load_persisted_pins(dir.path());
        assert!(on_disk.is_empty(), "bypass flush should not persist");
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn bypass_still_returns_resources() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease_disabled(dir.path());
        let key = ResourceKey::new("audio.mp3");

        let res = lease.open_resource(&key).unwrap();

        // Resource should still be usable
        res.write_at(0, b"hello").unwrap();
        let mut buf = [0u8; 5];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");
    }
}
