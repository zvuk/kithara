#![forbid(unsafe_code)]

use std::{future::Future, num::NonZeroUsize, ops::Range, path::Path, sync::atomic::AtomicU64};

use kithara_platform::{sync::Arc, tokio::sync::mpsc};
use rangemap::RangeSet;

#[cfg(not(target_arch = "wasm32"))]
use super::DiskStore;
use super::{AssetReader, MemStore, ResourceAcquisition};
#[cfg(not(target_arch = "wasm32"))]
use crate::backend::DiskAssetStore;
use crate::{
    decorator::{Assets, EvictionRouter, EvictionSubscription, ProcessCtx},
    error::AssetsResult,
    index::{
        AvailabilityIndex, DemandEntry, DemandIndex, DemandLease, ProducerHandle,
        ResourceTransactionIndex,
    },
    layout::{AssetLayoutRegistry, AssetScope, AssetSource, ResourceKey},
    resource::{AssetResourceState, RequestIdentity},
};

/// Forward a method call to the active store variant. Keeps the
/// `#[cfg(not(target_arch = "wasm32"))]` gate on `Disk` in one place so
/// the enum arms don't repeat it across a dozen trivial wrappers.
macro_rules! delegate_to_store {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match &$self.inner.backend {
            #[cfg(not(target_arch = "wasm32"))]
            StoreBackendInner::Disk { store, .. } => store.$method($($arg),*),
            StoreBackendInner::Memory { store } => store.$method($($arg),*),
        }
    };
}

/// Cheap shared handle for one asset-store identity.
#[derive(Clone, Debug)]
pub struct AssetStore {
    inner: Arc<AssetStoreInner>,
}

#[derive(Debug)]
pub(super) struct AssetStoreInner {
    pub(super) backend: StoreBackendInner,
    pub(super) availability: AvailabilityIndex,
    pub(super) demand: DemandIndex,
    pub(super) transactions: ResourceTransactionIndex,
    pub(super) eviction: EvictionRouter,
    pub(super) layouts: AssetLayoutRegistry,
}

#[derive(Debug)]
pub(super) enum StoreBackendInner {
    #[cfg(not(target_arch = "wasm32"))]
    Disk {
        store: DiskStore,
        base: Option<Arc<DiskAssetStore>>,
    },
    Memory {
        store: MemStore,
    },
}

impl AssetStore {
    pub(super) fn new_handle(inner: AssetStoreInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Acquire a resource explicitly for mutation.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    pub fn acquire_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<ResourceAcquisition> {
        delegate_to_store!(self, acquire_resource, key, identity)
    }

    /// Acquire a resource with processing context for an explicit write path.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    pub fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<ProcessCtx>,
    ) -> AssetsResult<ResourceAcquisition> {
        delegate_to_store!(self, acquire_resource_with_ctx, key, identity, ctx)
    }

    /// Attach a consumer's download demand for `key`.
    ///
    /// `read_pos` is shared with the consumer (the producer reads its
    /// advances directly); `look_ahead` of `None` requests the whole
    /// file as fast as possible. Returns a [`DemandLease`] the consumer
    /// must hold for the lifetime of its demand, plus a
    /// [`ProducerHandle`] to the single CAS-winning attacher only -- the
    /// winner drives the shared download task. See `CONTEXT.md`
    /// "Consumer Demand".
    pub fn attach_demand(
        &self,
        key: &ResourceKey,
        read_pos: Arc<AtomicU64>,
        look_ahead: Option<u64>,
    ) -> (DemandLease, Option<ProducerHandle>) {
        let entry = Arc::new(DemandEntry::new(read_pos, look_ahead));
        self.demand().attach_demand(key, entry)
    }

    /// Return the crate-private aggregate availability handle.
    pub(crate) fn availability(&self) -> &AvailabilityIndex {
        &self.inner.availability
    }

    /// Return a snapshot of byte ranges known to be available for the
    /// given resource.
    ///
    /// Fast path: aggregate lookup. Slow path: if the aggregate is
    /// empty for this key and `resource_state` reports
    /// `Committed { final_len: Some(len) }`, return `0..len` without
    /// seeding the aggregate (the observer does the actual seeding on
    /// open).
    #[must_use]
    pub fn available_ranges(&self, key: &ResourceKey) -> RangeSet<u64> {
        let ranges = self.availability().available_ranges(key);
        if !ranges.is_empty() {
            return ranges;
        }
        if let Ok(AssetResourceState::Committed {
            final_len: Some(len),
        }) = self.resource_state(key)
            && len > 0
        {
            let mut set = RangeSet::default();
            set.insert(0..len);
            return set;
        }
        ranges
    }

    /// Persist the in-memory byte-availability aggregate snapshot to
    /// disk. For an in-memory store this is a no-op.
    ///
    /// Checkpointing is always explicit — there is no `Drop` hook and
    /// no background flush timer. Callers decide when a consistent
    /// aggregate must survive a restart.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the persistent index resource cannot
    /// be opened or the atomic write fails.
    pub fn checkpoint(&self) -> AssetsResult<()> {
        match &self.inner.backend {
            #[cfg(not(target_arch = "wasm32"))]
            StoreBackendInner::Disk { base, .. } => {
                base.as_ref().map_or(Ok(()), |base| base.checkpoint())
            }
            StoreBackendInner::Memory { .. } => Ok(()),
        }
    }

    /// Return `true` when every byte in `range` is already present for
    /// the resource, or when the range is empty.
    ///
    /// Fast path checks the aggregate; slow path falls back to
    /// `resource_state` so pre-existing committed files on disk are
    /// discoverable even before the observer wires in.
    #[must_use]
    pub fn contains_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        if self.availability().contains_range(key, range.clone()) {
            return true;
        }
        if let Ok(AssetResourceState::Committed {
            final_len: Some(len),
        }) = self.resource_state(key)
        {
            return range.end <= len;
        }
        false
    }

    /// Delete the entire asset directory.
    ///
    /// # Errors
    /// Returns `AssetsError` if the directory cannot be removed.
    pub(crate) fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        delegate_to_store!(self, delete_asset, asset_root)
    }

    /// Return the crate-private aggregate demand handle.
    fn demand(&self) -> &DemandIndex {
        &self.inner.demand
    }

    fn transactions(&self) -> &ResourceTransactionIndex {
        &self.inner.transactions
    }

    /// Return the crate-private eviction-router handle.
    fn eviction(&self) -> &EvictionRouter {
        &self.inner.eviction
    }

    /// Return the committed final length of the resource, if known.
    #[must_use]
    pub fn final_len(&self, key: &ResourceKey) -> Option<u64> {
        if let Some(len) = self.availability().final_len(key) {
            return Some(len);
        }
        if let Ok(AssetResourceState::Committed {
            final_len: Some(len),
        }) = self.resource_state(key)
        {
            return Some(len);
        }
        None
    }

    fn layouts(&self) -> &AssetLayoutRegistry {
        &self.inner.layouts
    }

    /// Open a resource by key (no processing context).
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    pub fn open_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<AssetReader> {
        delegate_to_store!(self, open_resource, key, identity)
    }

    /// Open a resource with processing context.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    pub fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<ProcessCtx>,
    ) -> AssetsResult<AssetReader> {
        delegate_to_store!(self, open_resource_with_ctx, key, identity, ctx)
    }

    /// Remove a single resource from the store. The concrete store
    /// dispatches through the canonical asset deleter
    /// channel, which atomically clears the matching
    /// [`AvailabilityIndex`](crate::index) entry — so this method
    /// must not invalidate the index again.
    ///
    /// # Errors
    /// Returns `AssetsError` if the backing resource cannot be removed.
    pub fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        delegate_to_store!(self, remove_resource, key)
    }

    /// Request the in-memory LRU handle cache hold up to `media_items`
    /// media resources (e.g. the variant segment count). The store applies
    /// its own non-media headroom and hard cap, returning the capacity
    /// actually installed. Use on a private per-stream store only; resizing
    /// an app-wide shared store clobbers sibling streams.
    #[must_use]
    pub fn reserve_cache_for(&self, media_items: usize) -> NonZeroUsize {
        match &self.inner.backend {
            #[cfg(not(target_arch = "wasm32"))]
            StoreBackendInner::Disk { store, .. } => store.reserve_cache_for(media_items),
            StoreBackendInner::Memory { store } => store.reserve_cache_for(media_items),
        }
    }

    /// Inspect the current resource state.
    ///
    /// # Errors
    /// Returns `AssetsError` if the key is invalid or the backend cannot inspect.
    pub fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        delegate_to_store!(self, resource_state, key)
    }

    /// Return the root directory for the asset store.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        delegate_to_store!(self, root_dir)
    }

    /// Bind `source` to the layout registered for marker `T`.
    ///
    /// # Errors
    /// Returns an error when the source or layout-owned root is invalid.
    pub fn scope<T: 'static>(&self, source: &AssetSource) -> AssetsResult<AssetScope> {
        let layout = Arc::clone(self.layouts().layout::<T>());
        AssetScope::new(self.clone(), source, layout)
    }

    /// Subscribe to evictions under `asset_root`.
    ///
    /// When a [`ResourceKey`] under `asset_root` is invalidated, the evicted key is sent on `tx`.
    /// A single subscriber per `asset_root`, last-writer-wins. The returned
    /// [`EvictionSubscription`] guard deregisters on drop.
    pub fn subscribe_eviction(
        &self,
        asset_root: Arc<str>,
        tx: mpsc::UnboundedSender<ResourceKey>,
    ) -> EvictionSubscription {
        self.eviction().subscribe(asset_root, tx)
    }

    /// Serialize a closure per key across clones of this store. The closure
    /// must re-read state inside; separate stores are not coordinated. Waiting
    /// and running operations release the transaction when cancelled.
    /// Transactions are not reentrant: an operation must not acquire the same
    /// key again through this store.
    pub async fn with_resource_transaction<T, F, Fut>(&self, key: &ResourceKey, operation: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.transactions().run(key, operation).await
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{AssetStoreBuilder, StorageBackend};

    #[kithara::test]
    fn clone_shares_one_inner_identity() {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build();
        let clone = store.clone();

        assert!(Arc::ptr_eq(&store.inner, &clone.inner));
    }
}
