#![forbid(unsafe_code)]

use std::{
    collections::HashMap,
    fmt::{self, Debug},
    fs,
    ops::Range,
    path::Path,
    sync::{Arc, Weak},
};

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
    state: Mutex<AssetResourceState>,
    key: ResourceKey,
    registry: Weak<Mutex<HashMap<ResourceKey, Weak<Self>>>>,
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
    inner: Arc<A>,
    live: Arc<Mutex<HashMap<ResourceKey, Weak<LiveResource>>>>,
    cancel: CancellationToken,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    /// Shared pins index — same instance held by `EvictAssets` and
    /// `DiskAssetDeleter`. Mutations (`add` / `remove`) flush
    /// immediately via the index's internal best-effort persistence.
    pins: PinsIndex,
}

impl<A> Debug for LeaseAssets<A>
where
    A: Assets,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseAssets")
            .field("pins", &self.pins)
            .finish_non_exhaustive()
    }
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    pub(crate) fn new(inner: Arc<A>, cancel: CancellationToken, pins: PinsIndex) -> Self {
        Self::with_byte_recorder(inner, cancel, None, pins)
    }

    /// Persist the current pins snapshot to disk (best-effort).
    ///
    /// Mutations through [`PinsIndex::add`] / [`PinsIndex::remove`]
    /// already flush eagerly; this method is a passive flush kept for
    /// API compatibility with callers that want an explicit checkpoint.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the pins index resource cannot be
    /// written. No-op (returns `Ok`) when the lease layer is bypassed
    /// (capability inactive) or the index is ephemeral.
    pub fn flush_pins(&self) -> AssetsResult<()> {
        if !self.is_active() {
            return Ok(());
        }
        self.pins.flush()
    }

    fn is_active(&self) -> bool {
        self.inner
            .capabilities()
            .contains(crate::base::Capabilities::LEASE)
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

    fn pin(&self, asset_root: &str) -> AssetsResult<LeaseGuard> {
        // PinsIndex flushes immediately on every mutation; an `Err`
        // here means the pin did not reach disk and must propagate so
        // the caller can fail the resource open instead of silently
        // returning a non-durable lease.
        self.pins.add(asset_root)?;

        let pins = self.pins.clone();
        let ar = asset_root.to_string();
        let cancel = self.cancel.clone();

        Ok(LeaseGuard {
            inner: Some(Arc::new(LeaseGuardInner {
                on_drop: Box::new(move || {
                    if cancel.is_cancelled() {
                        return;
                    }
                    tracing::trace!(asset_root = %ar, "LeaseGuard::drop - removing pin");
                    if let Err(e) = pins.remove(&ar) {
                        tracing::warn!(
                            asset_root = %ar,
                            error = %e,
                            "LeaseGuard::drop: failed to persist unpin",
                        );
                    }
                }),
            })),
        })
    }

    /// Create with byte recorder for asset-size tracking.
    pub(crate) fn with_byte_recorder(
        inner: Arc<A>,
        cancel: CancellationToken,
        byte_recorder: Option<Arc<dyn ByteRecorder>>,
        pins: PinsIndex,
    ) -> Self {
        Self {
            byte_recorder,
            cancel,
            inner,
            pins,
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Resource wrapper that combines lease guard with byte recording on commit.
#[derive(Clone)]
pub struct LeaseResource<R: ResourceExt, L> {
    mode: AccessMode,
    _lease: L,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    drop_token: Option<Arc<()>>,
    live: Option<Arc<LiveResource>>,
    remove: Option<RemoveFn>,
    resource_key: Option<ResourceKey>,
    inner: R,
    asset_root: String,
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

impl<R, L> LeaseResource<crate::cache::CachedResource<R>, L>
where
    R: ResourceExt + Clone + Send + Sync + Debug + 'static,
{
    /// Unpin the underlying cached resource.
    pub fn release(self) -> Self {
        self.inner.set_released();
        self
    }

    /// Pin the underlying cached resource so it is never evicted.
    pub fn retain(self) -> Self {
        self.inner.set_retained();
        self
    }
}

impl<R, L> ResourceExt for LeaseResource<R, L>
where
    R: ResourceExt + Send + Sync + Clone + Debug + 'static,
    L: Send + Sync + Clone + 'static,
{
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

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.write_guard("write_at");
        self.inner.write_at(offset, data)
    }

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
            ResourceStatus::Active | ResourceStatus::Cancelled | ResourceStatus::Failed(_)
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
    type Context = A::Context;
    type IndexRes = A::IndexRes;
    type Res = LeaseResource<A::Res, LeaseGuard>;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.wrap_resource(key, ctx, AccessMode::Write)
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.wrap_resource(key, ctx, AccessMode::Read)
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.live.lock_sync().remove(key);
        self.inner.remove_resource(key)
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        // The `Arc<LiveResource>` upgraded out of `self.live` must NOT drop
        // while the registry mutex is still held: `LiveResource::Drop` re-locks
        // the same mutex, and parking_lot's `Mutex` is non-reentrant — that
        // path self-deadlocks the calling thread, which in turn freezes the
        // tokio runtime shutdown and trips the test-macro hard-timeout
        // watchdog (process::abort → SIGABRT) under `just test` contention.
        // Bind the upgrade through a separate `let` so the guard temporary
        // is dropped at the end of this statement, before any potential
        // Arc-drop in the body below.
        let live = self.live.lock_sync().get(key).and_then(Weak::upgrade);
        if let Some(live) = live {
            return Ok(live.snapshot());
        }
        self.inner.resource_state(key)
    }

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
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
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
                mode,
                _lease: LeaseGuard { inner: None },
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
            mode,
            _lease: lease,
            asset_root: self.inner.asset_root().to_string(),
            byte_recorder: self.byte_recorder.clone(),
            drop_token: matches!(mode, AccessMode::Write).then(|| Arc::new(())),
            live: Some(live),
            remove: Some(remove),
            resource_key: Some(key.clone()),
        })
    }

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
}

impl<A> Drop for LeaseAssets<A>
where
    A: Assets,
{
    fn drop(&mut self) {
        // Pins are flushed eagerly on every mutation through
        // `PinsIndex` itself; no per-clone bookkeeping needed here.
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
    use std::collections::HashSet;

    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{disk_store::DiskAssetStore, key::ResourceKey};

    fn make_pins_disk(dir: &Path) -> PinsIndex {
        let path = dir.join("_index").join("pins.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        PinsIndex::with_persist_at(path, CancellationToken::new(), crate::byte_pool())
    }

    fn make_lease(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
            crate::byte_pool(),
        ));
        let pins = make_pins_disk(dir);
        LeaseAssets::new(disk, CancellationToken::new(), pins)
    }

    /// Bypass test: empty `asset_root` → capabilities lack LEASE.
    fn make_lease_disabled(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "",
            CancellationToken::new(),
            crate::byte_pool(),
        ));
        let pins = make_pins_disk(dir);
        LeaseAssets::new(disk, CancellationToken::new(), pins)
    }

    fn load_persisted_pins(dir: &Path) -> HashSet<String> {
        // Reopen pins.bin and snapshot its on-disk state.
        let path = dir.join("_index").join("pins.bin");
        if !path.exists() {
            return HashSet::new();
        }
        let idx = PinsIndex::with_persist_at(path, CancellationToken::new(), crate::byte_pool());
        idx.snapshot()
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
    fn explicit_flush_after_pin_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::new("audio.mp3");

        let _res = lease.acquire_resource(&key).unwrap();

        // Mutations through `PinsIndex::add` already flush eagerly,
        // so an explicit flush is just a re-write of the same state.
        lease.flush_pins().unwrap();

        let on_disk = load_persisted_pins(dir.path());
        assert!(on_disk.contains("test_asset"));
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
        assert!(lease.pins.snapshot().is_empty(), "bypass should not pin");
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
