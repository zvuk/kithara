#![forbid(unsafe_code)]

use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Debug},
    fs,
    ops::Range,
    path::Path,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_bufpool::BytePool;
use kithara_platform::Mutex;
use kithara_storage::{ResourceExt, ResourceStatus, StorageResult, WaitOutcome};
use tokio_util::sync::CancellationToken;

use crate::{
    AssetResourceState, base::Assets, error::AssetsResult, evict::ByteRecorder, index::PinsIndex,
    key::ResourceKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessMode {
    Read,
    Write,
}

type RemoveFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

struct LiveResource {
    key: ResourceKey,
    registry: Weak<Mutex<HashMap<ResourceKey, Weak<Self>>>>,
    state: Mutex<AssetResourceState>,
}

impl LiveResource {
    fn new(
        key: ResourceKey,
        registry: Weak<Mutex<HashMap<ResourceKey, Weak<Self>>>>,
        state: AssetResourceState,
    ) -> Self {
        Self {
            key,
            registry,
            state: Mutex::new(state),
        }
    }

    fn set(&self, state: AssetResourceState) {
        *self.state.lock_sync() = state;
    }

    fn snapshot(&self) -> AssetResourceState {
        self.state.lock_sync().clone()
    }
}

impl Drop for LiveResource {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut guard = registry.lock_sync();
        if guard.get(&self.key).and_then(Weak::upgrade).is_none() {
            guard.remove(&self.key);
        }
    }
}

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
    live: Arc<Mutex<HashMap<ResourceKey, Weak<LiveResource>>>>,
    pins: Arc<Mutex<HashSet<String>>>,
    pool: BytePool,
}

impl<A> Debug for LeaseAssets<A>
where
    A: Assets,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pin_count = self.pins.try_lock().ok().map(|pins| pins.len());
        f.debug_struct("LeaseAssets")
            .field("dirty", &self.dirty.load(Ordering::Acquire))
            .field("pin_count", &pin_count)
            .finish_non_exhaustive()
    }
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
            live: Arc::new(Mutex::new(HashMap::new())),
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
            live: Arc::new(Mutex::new(HashMap::new())),
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

    fn open_live_resource(&self, key: &ResourceKey, status: ResourceStatus) -> Arc<LiveResource> {
        let next = AssetResourceState::from(status);
        let mut registry = self.live.lock_sync();
        if let Some(existing) = registry.get(key).and_then(Weak::upgrade) {
            let preserve_live = matches!(
                existing.snapshot(),
                AssetResourceState::Active | AssetResourceState::Failed(_)
            ) && matches!(next, AssetResourceState::Committed { .. });
            if !preserve_live {
                existing.set(next);
            }
            return existing;
        }

        let live = Arc::new(LiveResource::new(
            key.clone(),
            Arc::downgrade(&self.live),
            next,
        ));
        registry.insert(key.clone(), Arc::downgrade(&live));
        live
    }
}

/// Resource wrapper that combines lease guard with byte recording on commit.
#[derive(Clone)]
pub struct LeaseResource<R: ResourceExt, L> {
    inner: R,
    _lease: L,
    mode: AccessMode,
    asset_root: String,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    drop_token: Option<Arc<()>>,
    live: Option<Arc<LiveResource>>,
    remove: Option<RemoveFn>,
    resource_key: Option<ResourceKey>,
}

impl<R, L> Debug for LeaseResource<R, L>
where
    R: ResourceExt + Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseResource")
            .field("inner", &self.inner)
            .field("mode", &self.mode)
            .field("asset_root", &self.asset_root)
            .finish_non_exhaustive()
    }
}

impl<R: ResourceExt, L> LeaseResource<R, L> {
    fn write_guard(&self, op: &str) {
        assert!(
            matches!(self.mode, AccessMode::Write),
            "{op} requires acquire_resource*(); handle was opened via open_resource*()"
        );
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
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn status(&self) -> ResourceStatus;
            fn contains_range(&self, range: Range<u64>) -> bool;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
        }
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.write_guard("write_at");
        self.inner.write_at(offset, data)
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.write_guard("commit");
        self.inner.commit(final_len)?;
        if let Some(live) = &self.live {
            live.set(AssetResourceState::from(self.inner.status()));
        }

        // Record bytes if recorder is set (best-effort)
        if let Some(ref recorder) = self.byte_recorder
            && let Some(path) = self.inner.path()
            && let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
        {
            recorder.record_bytes(&self.asset_root, metadata.len());
        }

        Ok(())
    }

    fn fail(&self, reason: String) {
        self.write_guard("fail");
        self.inner.fail(reason.clone());
        if let Some(live) = &self.live {
            live.set(AssetResourceState::Failed(reason));
        }
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.write_guard("reactivate");
        self.inner.reactivate()?;
        if let Some(live) = &self.live {
            live.set(AssetResourceState::Active);
        }
        Ok(())
    }
}

impl<R, L> Drop for LeaseResource<R, L>
where
    R: ResourceExt,
{
    fn drop(&mut self) {
        if !matches!(self.mode, AccessMode::Write) {
            return;
        }

        if !matches!(
            self.inner.status(),
            ResourceStatus::Active | ResourceStatus::Failed(_)
        ) {
            return;
        }

        if let (Some(remove), Some(key)) = (&self.remove, &self.resource_key) {
            if self
                .drop_token
                .as_ref()
                .is_some_and(|token| Arc::strong_count(token) > 1)
            {
                return;
            }
            remove(key);
        }
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
        }
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        if let Some(live) = self.live.lock_sync().get(key).and_then(Weak::upgrade) {
            return Ok(live.snapshot());
        }
        self.inner.resource_state(key)
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.wrap_resource(key, ctx, AccessMode::Read)
    }

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.wrap_resource(key, ctx, AccessMode::Write)
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.live.lock_sync().remove(key);
        self.inner.remove_resource(key)
    }
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    fn wrap_resource(
        &self,
        key: &ResourceKey,
        ctx: Option<A::Context>,
        mode: AccessMode,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        let inner = match mode {
            AccessMode::Read => self.inner.open_resource_with_ctx(key, ctx)?,
            AccessMode::Write => self.inner.acquire_resource_with_ctx(key, ctx)?,
        };
        self.wrap_opened_resource(key, inner, mode)
    }

    fn wrap_opened_resource(
        &self,
        key: &ResourceKey,
        inner: A::Res,
        mode: AccessMode,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        let live = self.open_live_resource(key, inner.status());

        if !self.is_active() {
            return Ok(LeaseResource {
                inner,
                _lease: LeaseGuard { inner: None },
                mode,
                asset_root: self.inner.asset_root().to_string(),
                byte_recorder: None,
                drop_token: matches!(mode, AccessMode::Write).then(|| Arc::new(())),
                live: Some(live),
                remove: None,
                resource_key: Some(key.clone()),
            });
        }

        let lease = self.pin(self.inner.asset_root())?;
        let remove: RemoveFn = {
            let inner = Arc::clone(&self.inner);
            Arc::new(move |key: &ResourceKey| {
                let _ = inner.remove_resource(key);
            })
        };

        Ok(LeaseResource {
            inner,
            _lease: lease,
            mode,
            asset_root: self.inner.asset_root().to_string(),
            byte_recorder: self.byte_recorder.clone(),
            drop_token: matches!(mode, AccessMode::Write).then(|| Arc::new(())),
            live: Some(live),
            remove: Some(remove),
            resource_key: Some(key.clone()),
        })
    }

    /// Open a resource through the explicit mutable-access alias.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the inner store cannot open the resource.
    pub fn acquire_resource(
        &self,
        key: &ResourceKey,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        self.acquire_resource_with_ctx(key, None)
    }

    /// Open a resource with context through the explicit mutable-access alias.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the inner store cannot open the resource.
    pub fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<A::Context>,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        self.wrap_resource(key, ctx, AccessMode::Write)
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

        // Acquire a resource — this pins `test_asset`
        let _res = lease.acquire_resource(&key).unwrap();

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

        let _res = lease.acquire_resource(&key).unwrap();

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
        let res = lease.acquire_resource(&key).unwrap();

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
        let _res = lease.acquire_resource(&key).unwrap();

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
        let _res = lease_clone.acquire_resource(&key).unwrap();

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
        fs::write(&p, b"data").unwrap();
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
        fs::write(&p, b"data").unwrap();
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
        fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p);

        let res = lease.open_resource(&key).unwrap();

        // Resource should still be usable (read-only for absolute keys)
        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }
}
