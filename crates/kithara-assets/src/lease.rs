#![forbid(unsafe_code)]

use std::{
    fmt::{self, Debug},
    fs,
    ops::Range,
    path::Path,
    sync::{Arc, Weak},
};

use dashmap::DashMap;
use kithara_platform::Mutex;
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};
use tokio_util::sync::CancellationToken;

use crate::{
    AssetResourceState,
    base::{Assets, ResourceHandle},
    error::AssetsResult,
    evict::ByteRecorder,
    identity::RequestIdentity,
    index::PinsIndex,
    key::ResourceKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessMode {
    Read,
    Write,
}

type RemoveFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

/// Shared registry of live (non-dropped) lease resources keyed by
/// `ResourceKey` (which carries its own `asset_root`). Per-shard locks
/// via `DashMap`; contention is bounded by lease churn rate and shard
/// count (default 32).
type LiveRegistry = DashMap<ResourceKey, Weak<LiveResource>>;

struct LiveResource {
    state: Mutex<AssetResourceState>,
    key: ResourceKey,
    registry: Weak<LiveRegistry>,
}

impl LiveResource {
    fn new(key: ResourceKey, registry: Weak<LiveRegistry>, state: AssetResourceState) -> Self {
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
        registry.remove_if(&self.key, |_, weak| weak.upgrade().is_none());
    }
}

/// Decorator that adds "pin (lease) while handle lives" semantics on top of inner [`Assets`].
///
/// See crate `README.md` for the lease/pin contract. Absolute keys bypass
/// pinning (no asset to pin under). The capability gate also bypasses.
#[derive(Clone)]
pub struct LeaseAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    live: Arc<LiveRegistry>,
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

    fn is_active(&self) -> bool {
        self.inner
            .capabilities()
            .contains(crate::base::Capabilities::LEASE)
    }

    /// Force-persist the in-memory pin set to disk. No-op when the lease
    /// layer is bypassed (capability inactive) or the underlying pins
    /// index is ephemeral.
    ///
    /// # Errors
    /// Returns `AssetsError` if the underlying pins index flush fails.
    pub fn flush_pins(&self) -> AssetsResult<()> {
        if !self.is_active() {
            return Ok(());
        }
        self.pins.flush()
    }

    fn open_live_resource(&self, key: &ResourceKey, status: ResourceStatus) -> Arc<LiveResource> {
        let next = AssetResourceState::from(status);
        let entry = self.live.entry(key.clone());
        match entry {
            dashmap::mapref::entry::Entry::Occupied(mut occ) => {
                if let Some(existing) = occ.get().upgrade() {
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
                occ.insert(Arc::downgrade(&live));
                live
            }
            dashmap::mapref::entry::Entry::Vacant(vac) => {
                let live = Arc::new(LiveResource::new(
                    key.clone(),
                    Arc::downgrade(&self.live),
                    next,
                ));
                vac.insert(Arc::downgrade(&live));
                live
            }
        }
    }

    fn pin(&self, asset_root: &str) -> AssetsResult<LeaseGuard> {
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
            live: Arc::new(DashMap::new()),
        }
    }
}

/// Resource wrapper that combines lease guard with byte recording on commit.
#[derive(Clone)]
pub struct LeaseResource<R: ResourceHandle, L> {
    mode: AccessMode,
    _lease: L,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    drop_token: Option<Arc<()>>,
    live: Option<Arc<LiveResource>>,
    remove: Option<RemoveFn>,
    resource_key: Option<ResourceKey>,
    inner: R,
}

impl<R, L> Debug for LeaseResource<R, L>
where
    R: ResourceHandle + Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseResource")
            .field("inner", &self.inner)
            .field("mode", &self.mode)
            .field("key", &self.resource_key)
            .finish_non_exhaustive()
    }
}

impl<R: ResourceHandle, L> LeaseResource<R, L> {
    fn write_guard(&self, op: &str) {
        assert!(
            matches!(self.mode, AccessMode::Write),
            "{op} requires acquire_resource*(); handle was opened via open_resource*()"
        );
    }
}

impl<R, L> LeaseResource<crate::cache::CachedResource<R>, L>
where
    R: ResourceHandle + Clone + Send + Sync + Debug + 'static,
{
    /// Pin the underlying cached resource so it is never evicted.
    pub fn retain(self) -> Self {
        self.inner.set_retained();
        self
    }
}

impl<R, L> ResourceHandle for LeaseResource<R, L>
where
    R: ResourceHandle + Send + Sync + Clone + Debug + 'static,
    L: Send + Sync + Clone + 'static,
{
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.write_guard("commit");
        self.inner.commit(final_len)?;
        if let Some(live) = &self.live {
            live.set(AssetResourceState::from(self.inner.status()));
        }

        if let Some(ref recorder) = self.byte_recorder
            && let Some(asset_root) = self.resource_key.as_ref().and_then(ResourceKey::asset_root)
            && let Some(path) = self.inner.path()
            && let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
        {
            recorder.record_bytes(asset_root, metadata.len());
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
    R: ResourceHandle,
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
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.wrap_resource(key, identity, ctx, AccessMode::Write)
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.wrap_resource(key, identity, ctx, AccessMode::Read)
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.live.remove(key);
        self.inner.remove_resource(key)
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        let weak = self.live.get(key).map(|r| r.value().clone());
        if let Some(live) = weak.and_then(|w| w.upgrade()) {
            return Ok(live.snapshot());
        }
        self.inner.resource_state(key)
    }

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> crate::base::Capabilities;
            fn root_dir(&self) -> &Path;
            fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;
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
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        self.acquire_resource_with_ctx(key, identity, None)
    }

    /// Open a resource with context through the explicit mutable-access alias.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the inner store cannot open the resource.
    pub fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<A::Context>,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        self.wrap_resource(key, identity, ctx, AccessMode::Write)
    }

    fn wrap_opened_resource(
        &self,
        key: &ResourceKey,
        inner: A::Res,
        mode: AccessMode,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        let live = self.open_live_resource(key, inner.status());

        let pin = (self.is_active() && !key.is_absolute())
            .then(|| key.asset_root())
            .flatten();
        let Some(asset_root) = pin else {
            return Ok(LeaseResource {
                inner,
                mode,
                _lease: LeaseGuard { inner: None },
                byte_recorder: None,
                drop_token: matches!(mode, AccessMode::Write).then(|| Arc::new(())),
                live: Some(live),
                remove: None,
                resource_key: Some(key.clone()),
            });
        };

        let lease = self.pin(asset_root)?;
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
        identity: Option<&RequestIdentity>,
        ctx: Option<A::Context>,
        mode: AccessMode,
    ) -> AssetsResult<LeaseResource<A::Res, LeaseGuard>> {
        let inner = match mode {
            AccessMode::Read => self.inner.open_resource_with_ctx(key, identity, ctx)?,
            AccessMode::Write => self.inner.acquire_resource_with_ctx(key, identity, ctx)?,
        };
        self.wrap_opened_resource(key, inner, mode)
    }
}

impl<A> Drop for LeaseAssets<A>
where
    A: Assets,
{
    fn drop(&mut self) {}
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
    inner: Option<Arc<LeaseGuardInner>>,
}

impl LeaseGuard {
    /// `true` while at least one clone of this guard still pins the lease.
    /// `false` for no-op guards constructed when the lease is bypassed.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }
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

    const ROOT: &str = "test_asset";

    fn make_pins_disk(dir: &Path) -> PinsIndex {
        let path = dir.join("_index").join("pins.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        PinsIndex::with_persist_at(path, CancellationToken::new(), &crate::BytePool::default())
    }

    fn make_lease(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            CancellationToken::new(),
            &crate::BytePool::default(),
        ));
        let pins = make_pins_disk(dir);
        LeaseAssets::new(disk, CancellationToken::new(), pins)
    }

    fn load_persisted_pins(dir: &Path) -> HashSet<String> {
        let path = dir.join("_index").join("pins.bin");
        if !path.exists() {
            return HashSet::new();
        }
        let idx =
            PinsIndex::with_persist_at(path, CancellationToken::new(), &crate::BytePool::default());
        idx.snapshot()
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn pin_persists_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        let _res = lease.acquire_resource(&key, None).unwrap();

        let on_disk = load_persisted_pins(dir.path());
        assert!(
            on_disk.contains(ROOT),
            "pin should be persisted immediately"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn explicit_flush_after_pin_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        let _res = lease.acquire_resource(&key, None).unwrap();

        lease.flush_pins().unwrap();

        let on_disk = load_persisted_pins(dir.path());
        assert!(on_disk.contains(ROOT));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn drop_guard_eagerly_persists_unpin() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        let lease = make_lease(dir.path());
        let res = lease.acquire_resource(&key, None).unwrap();

        assert!(load_persisted_pins(dir.path()).contains(ROOT));

        drop(res);

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
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        let lease = make_lease(dir.path());
        let _res = lease.acquire_resource(&key, None).unwrap();

        lease.flush_pins().unwrap();
        let on_disk = load_persisted_pins(dir.path());
        assert!(on_disk.contains(ROOT), "flush should persist active pins");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn clone_pin_persists_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        let lease = make_lease(dir.path());
        let lease_clone = lease.clone();
        let _res = lease_clone.acquire_resource(&key, None).unwrap();

        assert!(
            load_persisted_pins(dir.path()).contains(ROOT),
            "pin via clone should be persisted immediately"
        );

        drop(lease_clone);

        assert!(
            load_persisted_pins(dir.path()).contains(ROOT),
            "pin should remain while resource handle is alive"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_does_not_pin_for_absolute_key() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let p = dir.path().join("audio.mp3");
        fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p);

        let _res = lease.open_resource(&key, None).unwrap();

        assert!(
            lease.pins.snapshot().is_empty(),
            "absolute key must bypass pinning"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_still_returns_resources() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let p = dir.path().join("audio.mp3");
        fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p);

        let res = lease.open_resource(&key, None).unwrap();

        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }
}
