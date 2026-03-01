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

use kithara_bufpool::BytePool;
use kithara_platform::Mutex;
use kithara_storage::{ResourceExt, ResourceStatus, StorageResult, WaitOutcome};
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
    inner: Arc<A>,
    pins: Arc<Mutex<HashSet<String>>>,
    pool: BytePool,
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    pub fn new(inner: Arc<A>, cancel: CancellationToken, pool: BytePool) -> Self {
        Self {
            byte_recorder: None,
            cancel,
            dirty: Arc::new(AtomicBool::new(false)),
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
            pool,
        }
    }

    /// Create with byte recorder for asset-size tracking.
    pub fn with_byte_recorder(
        inner: Arc<A>,
        cancel: CancellationToken,
        byte_recorder: Option<Arc<dyn ByteRecorder>>,
        pool: BytePool,
    ) -> Self {
        Self {
            byte_recorder,
            cancel,
            dirty: Arc::new(AtomicBool::new(false)),
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
            pool,
        }
    }

    pub(crate) fn inner(&self) -> &A {
        &self.inner
    }

    fn is_active(&self) -> bool {
        self.inner
            .capabilities()
            .contains(crate::base::Capabilities::LEASE)
    }

    fn open_index(&self) -> AssetsResult<PinsIndex<A::IndexRes>> {
        PinsIndex::open(self.inner(), self.pool.clone())
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
        let is_empty = self.pins.lock_sync().is_empty();

        if !is_empty {
            return Ok(());
        }

        let loaded = self.load_pins_best_effort()?;
        let mut guard = self.pins.lock_sync();
        for p in loaded {
            guard.insert(p);
        }
        drop(guard);
        Ok(())
    }

    /// Persist the current pin set to disk if dirty, then clear the dirty flag.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the pin index cannot be opened or written to disk.
    pub fn flush_pins(&self) -> AssetsResult<()> {
        if !self.is_active() || !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let snapshot = self.pins.lock_sync().clone();
        self.persist_pins_best_effort(&snapshot)
    }

    fn pin(&self, asset_root: &str) -> AssetsResult<LeaseGuard> {
        self.ensure_loaded_best_effort()?;

        {
            let mut pins = self.pins.lock_sync();
            if pins.insert(asset_root.to_string()) {
                self.dirty.store(true, Ordering::Release);
            }
        }

        // Eagerly persist so crash-recovery sees the pin.
        let _ = self.flush_pins();

        let owner = self.clone();
        let ar = asset_root.to_string();
        let cancel = self.cancel.clone();

        Ok(LeaseGuard {
            inner: Some(Arc::new(LeaseGuardInner {
                on_drop: Box::new(move || {
                    if cancel.is_cancelled() {
                        return;
                    }

                    tracing::trace!(asset_root = %ar, "LeaseGuard::drop - removing pin");

                    {
                        let mut pins = owner.pins.lock_sync();
                        if pins.remove(&ar) {
                            owner.dirty.store(true, Ordering::Release);
                        }
                    }

                    // Eagerly persist unpin so crash-recovery sees it.
                    let _ = owner.flush_pins();
                }),
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
    delegate::delegate! {
        to self.inner {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            fn fail(&self, reason: String);
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn reactivate(&self) -> StorageResult<()>;
            fn status(&self) -> ResourceStatus;
            fn contains_range(&self, range: Range<u64>) -> bool;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
        }
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
}

impl<A> Assets for LeaseAssets<A>
where
    A: Assets,
{
    type Res = LeaseResource<A::Res, LeaseGuard>;
    type Context = A::Context;
    type IndexRes = A::IndexRes;

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> crate::base::Capabilities;
            fn root_dir(&self) -> &Path;
            fn asset_root(&self) -> &str;
            fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn delete_asset(&self) -> AssetsResult<()>;
            fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()>;
        }
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        let inner = self.inner.open_resource_with_ctx(key, ctx)?;

        if !self.is_active() {
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
}

impl<A> Drop for LeaseAssets<A>
where
    A: Assets,
{
    fn drop(&mut self) {
        if !self.is_active() {
            return;
        }
        // Only persist on the last clone (all Arc fields share the same refcount via `pins`)
        if Arc::strong_count(&self.pins) == 1 && self.dirty.load(Ordering::Acquire) {
            let snapshot = self.pins.lock_sync().clone();
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
///
/// Non-generic: drop logic is captured as a closure for type erasure.
#[derive(Clone)]
pub struct LeaseGuard {
    #[expect(dead_code)]
    inner: Option<Arc<LeaseGuardInner>>,
}

struct LeaseGuardInner {
    on_drop: Box<dyn Fn() + Send + Sync>,
}

impl Drop for LeaseGuardInner {
    fn drop(&mut self) {
        (self.on_drop)();
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{disk_store::DiskAssetStore, index::PinsIndex, key::ResourceKey};

    fn make_lease(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
        ));
        LeaseAssets::new(disk, CancellationToken::new(), crate::byte_pool().clone())
    }

    /// Bypass test: empty asset_root → capabilities lack LEASE.
    fn make_lease_disabled(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(dir, "", CancellationToken::new()));
        LeaseAssets::new(disk, CancellationToken::new(), crate::byte_pool().clone())
    }

    fn load_persisted_pins(dir: &Path) -> HashSet<String> {
        let disk = DiskAssetStore::new(dir, "test_asset", CancellationToken::new());
        PinsIndex::open(&disk, crate::byte_pool().clone())
            .map_or_else(|_| HashSet::new(), |idx| idx.load().unwrap_or_default())
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_does_not_pin() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease_disabled(dir.path());
        let p = dir.path().join("audio.mp3");
        std::fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p);

        let _res = lease.open_resource(&key).unwrap();

        // Pins should be empty when capability absent
        assert!(lease.pins.lock_sync().is_empty(), "bypass should not pin");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_flush_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease_disabled(dir.path());
        let p = dir.path().join("audio.mp3");
        std::fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p);

        let _res = lease.open_resource(&key).unwrap();
        lease.flush_pins().unwrap();

        // Nothing should be persisted
        let on_disk = load_persisted_pins(dir.path());
        assert!(on_disk.is_empty(), "bypass flush should not persist");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_still_returns_resources() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease_disabled(dir.path());
        let p = dir.path().join("audio.mp3");
        std::fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p);

        let res = lease.open_resource(&key).unwrap();

        // Resource should still be usable (read-only for absolute keys)
        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }
}
