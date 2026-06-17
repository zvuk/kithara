#![forbid(unsafe_code)]

use std::{
    fmt::{self, Debug},
    fs,
    ops::Range,
    path::Path,
    sync::{Arc, Weak},
};

use dashmap::DashMap;
use kithara_platform::{CancelToken, sync::Mutex};
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use crate::{
    AssetResourceState,
    acquisition::{AcquisitionResult, RawWriteHandle, ReadSide, WriteSide},
    base::Assets,
    error::AssetsResult,
    evict::ByteRecorder,
    identity::RequestIdentity,
    index::PinsIndex,
    key::ResourceKey,
};

type RemoveFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

/// Pin guard + removal channel + byte recorder resolved for one resource key.
type LeaseBindings = (LeaseGuard, Option<RemoveFn>, Option<Arc<dyn ByteRecorder>>);

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
        *self.state.lock() = state;
    }

    fn snapshot(&self) -> AssetResourceState {
        self.state.lock().clone()
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
    cancel: CancelToken,
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
    pub(crate) fn new(inner: Arc<A>, cancel: CancelToken, pins: PinsIndex) -> Self {
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
        cancel: CancelToken,
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

/// Cleanup hook for an abandoned write handle. Lives as a field so
/// [`LeaseWriter`] has no `Drop` impl (its `commit`/`fail` partially move
/// `inner` out, which a `Drop` type forbids). If a writer is dropped without a
/// clean `commit`, this removes the partial resource — unless the resource was
/// committed in the meantime (checked via the shared [`LiveResource`] mirror).
struct WriterCleanup {
    remove: Option<RemoveFn>,
    resource_key: Option<ResourceKey>,
    drop_token: Option<Arc<()>>,
    live: Option<Arc<LiveResource>>,
    armed: bool,
}

impl WriterCleanup {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WriterCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(live) = &self.live
            && matches!(live.snapshot(), AssetResourceState::Committed { .. })
        {
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

/// Writer (Pending) phase of a leased resource. Pins the asset while alive,
/// records bytes + updates the live mirror on `commit`, and removes the partial
/// resource if dropped without committing.
pub struct LeaseWriter<W: WriteSide, L> {
    inner: W,
    _lease: L,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    /// Owns the removal channel, resource key, drop-token and live mirror —
    /// the single home for those, also read by `commit`/`reader`/`fail`.
    cleanup: WriterCleanup,
}

/// Reader (Ready) phase of a leased resource. Pins the asset while any clone is
/// alive; read-only. Carries the write-side machinery (`byte_recorder`,
/// `remove`) so [`reactivate`](ReadSide::reactivate) can rebuild a full writer.
pub struct LeaseReader<R: ReadSide, L> {
    inner: R,
    lease: L,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    remove: Option<RemoveFn>,
    live: Option<Arc<LiveResource>>,
    resource_key: Option<ResourceKey>,
}

impl<R, L> Clone for LeaseReader<R, L>
where
    R: ReadSide,
    L: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            lease: self.lease.clone(),
            byte_recorder: self.byte_recorder.clone(),
            remove: self.remove.clone(),
            live: self.live.clone(),
            resource_key: self.resource_key.clone(),
        }
    }
}

impl<W: WriteSide, L> Debug for LeaseWriter<W, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseWriter")
            .field("inner", &self.inner)
            .field("key", &self.cleanup.resource_key)
            .finish_non_exhaustive()
    }
}

impl<R: ReadSide, L> Debug for LeaseReader<R, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseReader")
            .field("inner", &self.inner)
            .field("key", &self.resource_key)
            .finish_non_exhaustive()
    }
}

impl<R, L> LeaseReader<crate::cache::CachedReader<R>, L>
where
    R: ReadSide,
{
    /// Pin the underlying cached resource so it is never evicted.
    pub fn retain(self) -> Self {
        self.inner.set_retained();
        self
    }
}

impl<W, L> LeaseWriter<crate::cache::CachedWriter<W>, L>
where
    W: WriteSide,
{
    /// Pin the underlying cached resource so it is never evicted.
    pub fn retain(self) -> Self {
        self.inner.set_retained();
        self
    }
}

impl<W, L> WriteSide for LeaseWriter<W, L>
where
    W: WriteSide,
    L: Send + Sync + Clone + 'static,
{
    type Reader = LeaseReader<W::Reader, L>;

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn reader(&self) -> LeaseReader<W::Reader, L> {
        LeaseReader {
            inner: self.inner.reader(),
            lease: self._lease.clone(),
            byte_recorder: self.byte_recorder.clone(),
            remove: self.cleanup.remove.clone(),
            live: self.cleanup.live.clone(),
            resource_key: self.cleanup.resource_key.clone(),
        }
    }

    fn raw_write_handle(&self) -> RawWriteHandle {
        self.inner.raw_write_handle()
    }

    fn commit(mut self, final_len: Option<u64>) -> StorageResult<LeaseReader<W::Reader, L>> {
        let reader_inner = self.inner.commit(final_len)?;
        if let Some(live) = &self.cleanup.live {
            live.set(AssetResourceState::from(reader_inner.status()));
        }
        if let Some(ref recorder) = self.byte_recorder
            && let Some(asset_root) = self
                .cleanup
                .resource_key
                .as_ref()
                .and_then(ResourceKey::asset_root)
            && let Some(path) = reader_inner.path()
            && let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
        {
            recorder.record_bytes(asset_root, metadata.len());
        }
        self.cleanup.disarm();
        Ok(LeaseReader {
            inner: reader_inner,
            lease: self._lease.clone(),
            byte_recorder: self.byte_recorder.clone(),
            remove: self.cleanup.remove.clone(),
            live: self.cleanup.live.clone(),
            resource_key: self.cleanup.resource_key.clone(),
        })
    }

    fn fail(mut self, reason: String) {
        self.inner.fail(reason.clone());
        if let Some(live) = &self.cleanup.live {
            live.set(AssetResourceState::Failed(reason));
        }
        // Explicit failure is a *terminal, observable* `Failed` state — disarm
        // the abandon-cleanup so the resource is not removed out from under a
        // `resource_state` query. Only a writer dropped WITHOUT commit/fail
        // (silent abandonment) triggers the partial-resource removal.
        self.cleanup.disarm();
    }
}

impl<R, L> ReadSide for LeaseReader<R, L>
where
    R: ReadSide,
    L: Send + Sync + Clone + 'static,
{
    type Writer = LeaseWriter<R::Writer, L>;

    fn reactivate(self) -> StorageResult<LeaseWriter<R::Writer, L>> {
        let writer_inner = self.inner.reactivate()?;
        if let Some(live) = &self.live {
            live.set(AssetResourceState::Active);
        }
        Ok(LeaseWriter {
            inner: writer_inner,
            _lease: self.lease,
            byte_recorder: self.byte_recorder,
            cleanup: WriterCleanup {
                remove: self.remove,
                resource_key: self.resource_key,
                drop_token: Some(Arc::new(())),
                live: self.live,
                armed: true,
            },
        })
    }

    delegate::delegate! {
        to self.inner {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn status(&self) -> ResourceStatus;
            fn contains_range(&self, range: Range<u64>) -> bool;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
        }
    }
}

impl<A> Assets for LeaseAssets<A>
where
    A: Assets,
{
    type ActiveRes = LeaseWriter<A::ActiveRes, LeaseGuard>;
    type Context = A::Context;
    type IndexRes = A::IndexRes;
    type ReadyRes = LeaseReader<A::ReadyRes, LeaseGuard>;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>> {
        match self.inner.acquire_resource_with_ctx(key, identity, ctx)? {
            AcquisitionResult::Pending(w) => {
                Ok(AcquisitionResult::Pending(self.wrap_writer(key, w)?))
            }
            AcquisitionResult::Ready(r) => Ok(AcquisitionResult::Ready(self.wrap_reader(key, r)?)),
        }
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::ReadyRes> {
        let inner = self.inner.open_resource_with_ctx(key, identity, ctx)?;
        self.wrap_reader(key, inner)
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
    /// Resolve the pin guard + removal channel + byte recorder for `key`.
    /// Absolute keys (and the bypass capability) yield a no-op lease.
    fn lease_for(&self, key: &ResourceKey) -> AssetsResult<LeaseBindings> {
        let pin = (self.is_active() && !key.is_absolute())
            .then(|| key.asset_root())
            .flatten();
        let Some(asset_root) = pin else {
            return Ok((LeaseGuard { inner: None }, None, None));
        };
        let lease = self.pin(asset_root)?;
        let remove: RemoveFn = {
            let inner = Arc::clone(&self.inner);
            Arc::new(move |key: &ResourceKey| {
                let _ = inner.remove_resource(key);
            })
        };
        Ok((lease, Some(remove), self.byte_recorder.clone()))
    }

    fn wrap_writer(
        &self,
        key: &ResourceKey,
        inner: A::ActiveRes,
    ) -> AssetsResult<LeaseWriter<A::ActiveRes, LeaseGuard>> {
        let live = self.open_live_resource(key, ResourceStatus::Active);
        let (lease, remove, byte_recorder) = self.lease_for(key)?;
        Ok(LeaseWriter {
            inner,
            _lease: lease,
            byte_recorder,
            cleanup: WriterCleanup {
                remove,
                resource_key: Some(key.clone()),
                drop_token: Some(Arc::new(())),
                live: Some(live),
                armed: true,
            },
        })
    }

    fn wrap_reader(
        &self,
        key: &ResourceKey,
        inner: A::ReadyRes,
    ) -> AssetsResult<LeaseReader<A::ReadyRes, LeaseGuard>> {
        let live = self.open_live_resource(key, inner.status());
        let (lease, remove, byte_recorder) = self.lease_for(key)?;
        Ok(LeaseReader {
            inner,
            lease,
            byte_recorder,
            remove,
            live: Some(live),
            resource_key: Some(key.clone()),
        })
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
        PinsIndex::with_persist_at(path, CancelToken::never(), &crate::BytePool::default())
    }

    fn make_lease(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            CancelToken::never(),
            &crate::BytePool::default(),
        ));
        let pins = make_pins_disk(dir);
        LeaseAssets::new(disk, CancelToken::never(), pins)
    }

    fn load_persisted_pins(dir: &Path) -> HashSet<String> {
        let path = dir.join("_index").join("pins.bin");
        if !path.exists() {
            return HashSet::new();
        }
        let idx =
            PinsIndex::with_persist_at(path, CancelToken::never(), &crate::BytePool::default());
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
