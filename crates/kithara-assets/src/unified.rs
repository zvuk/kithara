#![forbid(unsafe_code)]

use std::{
    fmt::Debug,
    hash::Hash,
    ops::Range,
    path::Path,
    sync::{Arc, atomic::AtomicU64},
};

#[cfg(test)]
use kithara_platform::CancelToken;
use rangemap::RangeSet;

use crate::{
    AssetResourceState,
    base::Assets,
    error::AssetsResult,
    identity::RequestIdentity,
    index::{AvailabilityIndex, DemandEntry, DemandIndex, DemandLease, ProducerHandle},
    key::ResourceKey,
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

/// Unified storage backend for assets.
///
/// Dispatches all operations to an inner disk or memory store chain.
/// The `availability` field holds the aggregate byte-availability index:
/// every query method below checks it first and falls back to a slow
/// `resource_state` probe only when the aggregate has no entry for the
/// key. The storage observer populates the aggregate on `write_at` /
/// `commit`; explicit persistence is via [`AssetStore::checkpoint`].
///
/// The `base` field on the `Disk` variant is an optional
/// [`Arc<DiskAssetStore>`] needed only to open `_index/availability.bin`
/// for checkpointing. It is `Some` when the store is built through
/// [`crate::AssetStoreBuilder::build`] (production path) and `None`
/// when a bare [`DiskStore`] chain is converted via `From`/`Into`
/// (test-only path). A `None` `base` makes `checkpoint()` a no-op.
#[derive(Clone, Debug)]
pub enum AssetStore<Ctx = ()>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    /// File-backed storage with mmap resources.
    #[cfg(not(target_arch = "wasm32"))]
    Disk {
        store: DiskStore<Ctx>,
        availability: AvailabilityIndex,
        demand: DemandIndex,
        base: Option<Arc<DiskAssetStore>>,
    },
    /// In-memory storage (ephemeral, no disk artifacts).
    Mem {
        store: MemStore<Ctx>,
        availability: AvailabilityIndex,
        demand: DemandIndex,
    },
}

impl<Ctx> AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    /// Acquire a resource explicitly for mutation.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    pub fn acquire_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<AssetResource<Ctx>> {
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
        ctx: Option<Ctx>,
    ) -> AssetsResult<AssetResource<Ctx>> {
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

    /// Whether this backend is ephemeral (in-memory).
    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Mem { .. })
    }

    /// Open a resource by key (no processing context).
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    pub fn open_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<AssetReader<Ctx>> {
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
        ctx: Option<Ctx>,
    ) -> AssetsResult<AssetReader<Ctx>> {
        delegate_to_store!(self, open_resource_with_ctx, key, identity, ctx)
    }

    /// Remove a single resource from the store. The concrete store
    /// dispatches through the canonical [`crate::deleter::AssetDeleter`]
    /// channel, which atomically clears the matching
    /// [`AvailabilityIndex`](crate::index) entry — so this method
    /// must not invalidate the index again.
    pub fn remove_resource(&self, key: &ResourceKey) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => {
                let _ = store.remove_resource(key);
            }
            Self::Mem { store, .. } => {
                let _ = store.remove_resource(key);
            }
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
    /// roots cooperate on one store.
    #[must_use]
    pub fn scope<R: Into<Arc<str>>>(&self, asset_root: R) -> AssetScope<Ctx> {
        AssetScope::new(self.clone(), asset_root.into())
    }
}

// Test-only conversions from a bare store chain (no shared cancel, no
// `base` for checkpointing). The `demand` index is constructed with a
// fresh cancel here because there is no upstream master on this path;
// production stores are built through `AssetStoreBuilder::build`, which
// threads the store cancel. Gated to keep the cancel-hierarchy lint
// honest: these have no non-test callers in the workspace.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
impl<Ctx> From<DiskStore<Ctx>> for AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: DiskStore<Ctx>) -> Self {
        Self::Disk {
            store,
            availability: AvailabilityIndex::new(),
            demand: DemandIndex::new(CancelToken::never()),
            base: None,
        }
    }
}

#[cfg(test)]
impl<Ctx> From<MemStore<Ctx>> for AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: MemStore<Ctx>) -> Self {
        Self::Mem {
            store,
            availability: AvailabilityIndex::new(),
            demand: DemandIndex::new(CancelToken::never()),
        }
    }
}
