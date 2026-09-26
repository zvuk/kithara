#![forbid(unsafe_code)]

use std::{num::NonZeroUsize, path::Path};

use dashmap::{DashMap, mapref::entry::Entry};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_storage::ResourceStatus;

use super::{
    guard::LeaseGuard,
    handle::{LeaseReader, LeaseWriter},
    live::{LiveRegistry, LiveResource, RemoveFn},
};
use crate::{
    AssetEvent,
    decorator::{Assets, ByteRecorder, CachedAssets, Capabilities, ProcessCtx},
    error::AssetsResult,
    index::{PinDurability, PinsIndex},
    layout::ResourceKey,
    resource::{AcquisitionResult, AssetResourceState, ReadSide, RequestIdentity},
};

#[derive(Clone, Default)]
pub(crate) struct LeaseEvents {
    bus: Option<EventBus>,
}

impl LeaseEvents {
    pub(crate) const fn new(bus: Option<EventBus>) -> Self {
        Self { bus }
    }

    pub(super) fn publish_committed(&self, key: Option<&ResourceKey>, final_len: Option<u64>) {
        let Some(bus) = &self.bus else {
            return;
        };
        let Some((asset_root, rel_path)) = key.and_then(|key| key.asset_root().zip(key.rel_path()))
        else {
            return;
        };
        bus.publish(AssetEvent::Committed {
            final_len,
            asset_root: asset_root.to_string(),
            rel_path: rel_path.to_string(),
        });
    }

    pub(super) fn publish_failed(&self, key: Option<&ResourceKey>, reason: &str) {
        let Some(bus) = &self.bus else {
            return;
        };
        let Some((asset_root, rel_path)) = key.and_then(|key| key.asset_root().zip(key.rel_path()))
        else {
            return;
        };
        bus.publish(AssetEvent::Failed {
            asset_root: asset_root.to_string(),
            rel_path: rel_path.to_string(),
            reason: reason.to_string(),
        });
    }
}

/// Pin guard + removal channel + byte recorder resolved for one resource key.
type LeaseBindings = (LeaseGuard, Option<RemoveFn>, Option<Arc<dyn ByteRecorder>>);

/// Decorator that adds "pin (lease) while handle lives" semantics on top of inner [`Assets`].
///
/// Absolute keys bypass pinning (no asset to pin under). The capability gate also
/// bypasses.
#[derive(Clone, derive_more::Debug)]
pub struct LeaseAssets<A> {
    #[debug(skip)]
    inner: Arc<A>,
    #[debug(skip)]
    live: Arc<LiveRegistry>,
    #[debug(skip)]
    cancel: CancelToken,
    #[debug(skip)]
    event_bus: LeaseEvents,
    #[debug(skip)]
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    /// Shared pins index — same instance held by `EvictAssets` and
    /// `DiskAssetDeleter`. A writer's pin is durable and flushes on its
    /// 0→1 / 1→0 transitions; a reader's pin is process-local and stays
    /// in memory, so the decoder read loop never reaches disk.
    pins: PinsIndex,
}

impl<A> LeaseAssets<CachedAssets<A>>
where
    A: Assets<Context = ProcessCtx>,
{
    pub(crate) fn cache_capacity(&self) -> NonZeroUsize {
        self.inner.cache_capacity()
    }
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    fn is_active(&self) -> bool {
        self.inner.capabilities().contains(Capabilities::LEASE)
    }

    fn open_live_resource(&self, key: &ResourceKey, status: ResourceStatus) -> Arc<LiveResource> {
        let next = AssetResourceState::from(status);
        let entry = self.live.entry(key.clone());
        match entry {
            Entry::Occupied(mut occ) => {
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
            Entry::Vacant(vac) => {
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

    fn pin(&self, asset_root: &str, durability: PinDurability) -> AssetsResult<LeaseGuard> {
        self.pins.add(asset_root, durability)?;

        let pins = self.pins.clone();
        let ar = asset_root.to_string();
        let cancel = self.cancel.clone();

        Ok(LeaseGuard::new(move || {
            if cancel.is_cancelled() {
                return;
            }
            tracing::trace!(asset_root = %ar, "LeaseGuard::drop - removing pin");
            if let Err(e) = pins.remove(&ar, durability) {
                tracing::warn!(
                    asset_root = %ar,
                    error = %e,
                    "LeaseGuard::drop: failed to persist unpin",
                );
            }
        }))
    }

    /// Create with byte recorder for asset-size tracking.
    pub(crate) fn with_byte_recorder(
        inner: Arc<A>,
        cancel: CancelToken,
        byte_recorder: Option<Arc<dyn ByteRecorder>>,
        event_bus: LeaseEvents,
        pins: PinsIndex,
    ) -> Self {
        Self {
            byte_recorder,
            cancel,
            event_bus,
            inner,
            pins,
            live: Arc::new(DashMap::new()),
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
            fn capabilities(&self) -> Capabilities;
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
    fn lease_for(
        &self,
        key: &ResourceKey,
        durability: PinDurability,
    ) -> AssetsResult<LeaseBindings> {
        let pin = (self.is_active() && !key.is_absolute())
            .then(|| key.asset_root())
            .flatten();
        let Some(asset_root) = pin else {
            return Ok((LeaseGuard::inactive(), None, None));
        };
        let lease = self.pin(asset_root, durability)?;
        let remove: RemoveFn = {
            let inner = Arc::clone(&self.inner);
            Arc::new(move |key: &ResourceKey| {
                let _ = inner.remove_resource(key);
            })
        };
        Ok((lease, Some(remove), self.byte_recorder.clone()))
    }

    fn wrap_reader(
        &self,
        key: &ResourceKey,
        inner: A::ReadyRes,
    ) -> AssetsResult<LeaseReader<A::ReadyRes, LeaseGuard>> {
        let live = self.open_live_resource(key, inner.status());
        let (lease, remove, byte_recorder) = self.lease_for(key, PinDurability::Local)?;
        Ok(LeaseReader::new(
            inner,
            lease,
            byte_recorder,
            self.event_bus.clone(),
            Some(live),
            remove,
            Some(key.clone()),
        ))
    }

    fn wrap_writer(
        &self,
        key: &ResourceKey,
        inner: A::ActiveRes,
    ) -> AssetsResult<LeaseWriter<A::ActiveRes, LeaseGuard>> {
        let live = self.open_live_resource(key, ResourceStatus::Active);
        let (lease, remove, byte_recorder) = self.lease_for(key, PinDurability::Durable)?;
        Ok(LeaseWriter::new(
            inner,
            lease,
            byte_recorder,
            self.event_bus.clone(),
            Some(live),
            remove,
            Some(key.clone()),
        ))
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::{collections::HashSet, fs};

    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{backend::DiskAssetStore, consts, layout::ResourceKey, resource::WriteSide};

    fn make_pins_disk(dir: &Path) -> PinsIndex {
        let pools = crate::test_pools::pools();
        let path = dir.join("_index").join("pins.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        PinsIndex::with_persist_at(path, crate::test_pools::byte_buffer(&pools))
    }

    fn make_lease(dir: &Path) -> LeaseAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(dir, CancelToken::never()));
        let pins = make_pins_disk(dir);
        LeaseAssets::with_byte_recorder(
            disk,
            CancelToken::never(),
            None,
            LeaseEvents::new(None),
            pins,
        )
    }

    fn load_persisted_pins(dir: &Path) -> HashSet<String> {
        let pools = crate::test_pools::pools();
        let path = dir.join("_index").join("pins.bin");
        if !path.exists() {
            return HashSet::new();
        }
        let idx = PinsIndex::with_persist_at(path, crate::test_pools::byte_buffer(&pools));
        idx.snapshot()
    }

    /// Write `payload` under `key` and commit it, leaving no live handle.
    fn commit_resource(lease: &LeaseAssets<DiskAssetStore>, key: &ResourceKey, payload: &[u8]) {
        let AcquisitionResult::Pending(writer) = lease.acquire_resource(key, None).unwrap() else {
            panic!("a fresh key must acquire a writer");
        };
        writer.write_at(0, payload).unwrap();
        let len = u64::try_from(payload.len()).unwrap();
        drop(writer.commit(Some(len)).unwrap());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reading_a_committed_resource_does_not_write_the_pins_index() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::relative(consts::LAYER_ROOT, "audio.mp3");
        commit_resource(&lease, &key, b"data");

        let path = dir.path().join("_index").join("pins.bin");
        fs::remove_file(&path).unwrap();

        for _ in 0..16 {
            let reader = lease.open_resource(&key, None).unwrap();
            let mut buf = [0u8; 4];
            reader.read_at(0, &mut buf).unwrap();
        }

        assert!(
            !path.exists(),
            "reads of a committed resource must not reach the pins index on disk"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reader_lease_still_guards_the_asset_against_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::relative(consts::LAYER_ROOT, "audio.mp3");
        commit_resource(&lease, &key, b"data");

        let _reader = lease.open_resource(&key, None).unwrap();

        assert!(
            lease.pins.snapshot().contains(consts::LAYER_ROOT),
            "a live reader must keep its asset root pinned in memory"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn pin_persists_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let key = ResourceKey::relative(consts::LAYER_ROOT, "audio.mp3");

        let _res = lease.acquire_resource(&key, None).unwrap();

        let on_disk = load_persisted_pins(dir.path());
        assert!(
            on_disk.contains(consts::LAYER_ROOT),
            "pin should be persisted immediately"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn drop_guard_eagerly_persists_unpin() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::relative(consts::LAYER_ROOT, "audio.mp3");

        let lease = make_lease(dir.path());
        let res = lease.acquire_resource(&key, None).unwrap();

        assert!(load_persisted_pins(dir.path()).contains(consts::LAYER_ROOT));

        drop(res);

        let on_disk = load_persisted_pins(dir.path());
        assert!(
            on_disk.is_empty(),
            "unpin should be eagerly persisted, got {:?}",
            on_disk
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn clone_pin_persists_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let key = ResourceKey::relative(consts::LAYER_ROOT, "audio.mp3");

        let lease = make_lease(dir.path());
        let lease_clone = lease.clone();
        drop(lease);
        let _res = lease_clone.acquire_resource(&key, None).unwrap();

        assert!(
            load_persisted_pins(dir.path()).contains(consts::LAYER_ROOT),
            "pin via clone should be persisted immediately"
        );

        drop(lease_clone);

        assert!(
            load_persisted_pins(dir.path()).contains(consts::LAYER_ROOT),
            "pin should remain while resource handle is alive"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_does_not_pin_for_absolute_key() {
        let dir = tempfile::tempdir().unwrap();
        let lease = make_lease(dir.path());
        let p = dir.path().join("audio.mp3");
        fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p).expect("absolute test path");

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
        let key = ResourceKey::absolute(&p).expect("absolute test path");

        let res = lease.open_resource(&key, None).unwrap();

        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }
}
