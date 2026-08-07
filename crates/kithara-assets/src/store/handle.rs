#![forbid(unsafe_code)]

use std::{future::Future, num::NonZeroUsize, ops::Range, path::Path, sync::atomic::AtomicU64};

use kithara_platform::{sync::Arc, tokio::sync::mpsc};
use rangemap::RangeSet;

#[cfg(not(target_arch = "wasm32"))]
use super::DiskStore;
use super::{AssetReader, MemStore, ResourceAcquisition};
#[cfg(not(target_arch = "wasm32"))]
use crate::backend::DiskAssetStore;
#[cfg(test)]
use crate::decorator::Capabilities;
use crate::{
    decorator::{Assets, EvictionRouter, EvictionSubscription, ProcessCtx},
    error::{AssetsError, AssetsResult},
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
    pub(super) layouts: AssetLayoutRegistry,
    pub(super) availability: AvailabilityIndex,
    pub(super) demand: DemandIndex,
    pub(super) eviction: EvictionRouter,
    pub(super) transactions: ResourceTransactionIndex,
    pub(super) backend: StoreBackendInner,
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
    #[cfg(test)]
    pub(super) fn capabilities(&self) -> Capabilities {
        delegate_to_store!(self, capabilities)
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

    delegate::delegate! {
        to self.inner {
            /// Return the crate-private aggregate availability handle.
            #[field(&availability)]
            pub(crate) fn availability(&self) -> &AvailabilityIndex;
            /// Return the crate-private aggregate demand handle.
            #[field(&demand)]
            fn demand(&self) -> &DemandIndex;
            /// Return the crate-private eviction-router handle.
            #[field(&eviction)]
            fn eviction(&self) -> &EvictionRouter;
            #[field(&layouts)]
            fn layouts(&self) -> &AssetLayoutRegistry;
            #[field(&transactions)]
            fn transactions(&self) -> &ResourceTransactionIndex;
        }
    }

    /// Return a snapshot of byte ranges known to be available for the
    /// given resource, answered from the availability aggregate.
    #[must_use]
    pub fn available_ranges(&self, key: &ResourceKey) -> RangeSet<u64> {
        self.availability().available_ranges(key)
    }

    /// Persist the in-memory byte-availability aggregate snapshot to
    /// disk. For an in-memory store this is a no-op.
    ///
    /// Callers can checkpoint at any point they want a consistent
    /// aggregate on disk; the store also checkpoints itself when the last
    /// handle drops, because the manifest is what makes a resource usable
    /// after a restart.
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
    /// Answers from the availability aggregate alone, which keeps the
    /// probe free of store locks and filesystem calls — it runs inside
    /// `rtsan_forbid_blocking` regions. Hydration at store build and the
    /// write/commit observers keep the aggregate complete; a resource the
    /// aggregate does not know is absent and gets refetched.
    #[must_use]
    pub fn contains_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        self.availability().contains_range(key, range)
    }

    /// Delete the entire asset directory.
    ///
    /// # Errors
    /// Returns `AssetsError` if the directory cannot be removed.
    pub(crate) fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        delegate_to_store!(self, delete_asset, asset_root)
    }

    /// Return the fixed handle-cache capacity for an ephemeral memory store.
    /// Durable stores return `None` because handle displacement does not remove
    /// their committed bytes.
    #[must_use]
    pub fn ephemeral_cache_capacity(&self) -> Option<NonZeroUsize> {
        match &self.inner.backend {
            #[cfg(not(target_arch = "wasm32"))]
            StoreBackendInner::Disk { .. } => None,
            StoreBackendInner::Memory { store } => Some(store.cache_capacity()),
        }
    }

    /// Return the committed final length of the resource, if known to
    /// the availability aggregate.
    #[must_use]
    pub fn final_len(&self, key: &ResourceKey) -> Option<u64> {
        self.availability().final_len(key)
    }

    /// Return whether both handles refer to the same store instance.
    #[must_use]
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(super) fn new_handle(inner: AssetStoreInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
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
        if key.is_absolute() {
            return Err(AssetsError::InvalidKey);
        }
        delegate_to_store!(self, remove_resource, key)
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
    /// Every subscriber for that root receives the key. The returned
    /// [`EvictionSubscription`] guard deregisters only its own subscription on drop.
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

/// The manifest decides what survives a restart, so the last handle writes
/// it before the indexes go away. Waiting for the flush hub's own teardown
/// is too late: every index holds the hub alive, so by the time the hub
/// drops there is nothing left to flush.
impl Drop for AssetStoreInner {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let StoreBackendInner::Disk {
            base: Some(base), ..
        } = &self.backend
            && let Err(error) = base.checkpoint()
        {
            tracing::warn!(%error, "AssetStore: final checkpoint failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use crate::{AssetStore, StorageBackend};

    #[kithara::test]
    fn clone_shares_one_inner_identity() {
        let store = AssetStore::builder()
            .backend(StorageBackend::Memory)
            .build();
        let clone = store.clone();
        let other = AssetStore::builder()
            .backend(StorageBackend::Memory)
            .build();

        assert!(store.is_same(&clone));
        assert!(!store.is_same(&other));
    }
}
