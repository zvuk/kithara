#![forbid(unsafe_code)]

use std::{future::Future, num::NonZeroUsize, ops::Range, path::Path, sync::atomic::AtomicU64};

#[cfg(test)]
use kithara_platform::CancelToken;
use kithara_platform::{sync::Arc, tokio::sync::mpsc};
use rangemap::RangeSet;

use crate::{
    AssetResourceState,
    base::Assets,
    error::AssetsResult,
    eviction::{EvictionRouter, EvictionSubscription},
    identity::RequestIdentity,
    index::{
        AvailabilityIndex, DemandEntry, DemandIndex, DemandLease, ProducerHandle,
        ResourceTransactionIndex,
    },
    key::ResourceKey,
    layout::AssetLayout,
    process::ProcessCtx,
    scope::AssetScope,
    store::{AssetReader, AssetResource, MemStore},
};
#[cfg(not(target_arch = "wasm32"))]
use crate::{disk_store::DiskAssetStore, store::DiskStore};

/// Forward a method call to the active store variant. Keeps the
/// `#[cfg(not(target_arch = "wasm32"))]` gate on `Disk` in one place so
/// the enum arms don't repeat it across a dozen trivial wrappers.
macro_rules! delegate_to_store {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.$method($($arg),*),
            Self::Mem { store, .. } => store.$method($($arg),*),
        }
    };
}

/// Unified storage backend: dispatches to an inner disk or memory store chain.
/// Byte-availability queries hit the aggregate `availability` index first, else
/// a slow `resource_state` probe; persistence is via [`AssetStore::checkpoint`].
#[derive(Clone, Debug)]
pub enum AssetStore {
    /// File-backed storage with mmap resources.
    #[cfg(not(target_arch = "wasm32"))]
    Disk {
        store: DiskStore,
        availability: AvailabilityIndex,
        demand: DemandIndex,
        transactions: ResourceTransactionIndex,
        eviction: EvictionRouter,
        base: Option<Arc<DiskAssetStore>>,
        layout: Arc<dyn AssetLayout>,
    },
    /// In-memory storage (ephemeral, no disk artifacts).
    Mem {
        store: MemStore,
        availability: AvailabilityIndex,
        demand: DemandIndex,
        transactions: ResourceTransactionIndex,
        eviction: EvictionRouter,
        layout: Arc<dyn AssetLayout>,
    },
}

impl AssetStore {
    /// Acquire a resource explicitly for mutation.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    pub fn acquire_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<AssetResource> {
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
    ) -> AssetsResult<AssetResource> {
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
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { availability, .. } => availability,
            Self::Mem { availability, .. } => availability,
        }
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
    /// disk. For `AssetStore::Mem` this is a no-op.
    ///
    /// Checkpointing is always explicit — there is no `Drop` hook and
    /// no background flush timer. Callers decide when a consistent
    /// aggregate must survive a restart.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the persistent index resource cannot
    /// be opened or the atomic write fails.
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn checkpoint(&self) -> AssetsResult<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { base, .. } => base.as_ref().map_or(Ok(()), |base| base.checkpoint()),
            Self::Mem { .. } => Ok(()),
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
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { demand, .. } => demand,
            Self::Mem { demand, .. } => demand,
        }
    }

    fn transactions(&self) -> &ResourceTransactionIndex {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { transactions, .. } => transactions,
            Self::Mem { transactions, .. } => transactions,
        }
    }

    /// Return the crate-private eviction-router handle.
    fn eviction(&self) -> &EvictionRouter {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { eviction, .. } => eviction,
            Self::Mem { eviction, .. } => eviction,
        }
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

    /// The on-disk [`AssetLayout`] this store was built with.
    #[must_use]
    pub fn layout(&self) -> &Arc<dyn AssetLayout> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { layout, .. } => layout,
            Self::Mem { layout, .. } => layout,
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
    /// dispatches through the canonical [`crate::deleter::AssetDeleter`]
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
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.reserve_cache_for(media_items),
            Self::Mem { store, .. } => store.reserve_cache_for(media_items),
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

    /// Bind this store to one `asset_root`, returning a scoped handle
    /// that drops the per-call `asset_root` argument. Cheap to clone --
    /// the backing store is shared, so many scopes over distinct asset
    /// roots cooperate on one store; the scope inherits the store's [`AssetLayout`].
    #[must_use]
    pub fn scope<R: Into<Arc<str>>>(&self, asset_root: R) -> AssetScope {
        AssetScope::new(self.clone(), asset_root.into())
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

// Test-only conversions from a bare store chain (no shared cancel, no
// `base` for checkpointing). The `demand` index is constructed with a
// fresh cancel here because there is no upstream master on this path;
// production stores are built through `AssetStoreBuilder::build`, which
// threads the store cancel. Gated to keep the cancel-hierarchy lint
// Test-only: no non-test callers in the workspace. The `EvictionRouter`
// here is unwired (no `on_invalidated` hook); `subscribe_eviction` is a
// no-op on a `From`-built store. Production stores come from `build()`.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
impl From<DiskStore> for AssetStore {
    fn from(store: DiskStore) -> Self {
        Self::Disk {
            store,
            availability: AvailabilityIndex::new(),
            demand: DemandIndex::new(CancelToken::never()),
            transactions: ResourceTransactionIndex::default(),
            eviction: EvictionRouter::default(),
            base: None,
            layout: Arc::new(crate::layout::DefaultLayout),
        }
    }
}

#[cfg(test)]
impl From<MemStore> for AssetStore {
    fn from(store: MemStore) -> Self {
        Self::Mem {
            store,
            availability: AvailabilityIndex::new(),
            demand: DemandIndex::new(CancelToken::never()),
            transactions: ResourceTransactionIndex::default(),
            eviction: EvictionRouter::default(),
            layout: Arc::new(crate::layout::DefaultLayout),
        }
    }
}
