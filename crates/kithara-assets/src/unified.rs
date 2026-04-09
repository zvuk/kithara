#![forbid(unsafe_code)]

//! Unified asset store: disk or memory backend.

use std::{fmt::Debug, hash::Hash, ops::Range, path::Path};

use rangemap::RangeSet;

#[cfg(not(target_arch = "wasm32"))]
use crate::store::DiskStore;
use crate::{
    AssetResourceState,
    base::Assets,
    error::AssetsResult,
    index::AvailabilityIndex,
    key::ResourceKey,
    store::{AssetResource, MemStore},
};

/// Unified storage backend for assets.
///
/// Dispatches all operations to an inner disk or memory store chain.
/// The `availability` field holds the aggregate byte-availability index
/// (Phase P-2): every query method below checks it first and falls back
/// to a slow `resource_state` probe only when the aggregate has no entry
/// for the key. Phase P-3 wires the storage observer that populates the
/// aggregate on `write_at` / `commit`; Phase P-4 adds persistence.
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
    },
    /// In-memory storage (ephemeral, no disk artifacts).
    Mem {
        store: MemStore<Ctx>,
        availability: AvailabilityIndex,
    },
}

impl<Ctx> AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    /// Open a resource by key (no processing context).
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened.
    pub fn open_resource(&self, key: &ResourceKey) -> AssetsResult<AssetResource<Ctx>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.open_resource(key),
            Self::Mem { store, .. } => store.open_resource(key),
        }
    }

    /// Acquire a resource explicitly for mutation.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened.
    pub fn acquire_resource(&self, key: &ResourceKey) -> AssetsResult<AssetResource<Ctx>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.acquire_resource(key),
            Self::Mem { store, .. } => store.acquire_resource(key),
        }
    }

    /// Open a resource with processing context.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened.
    pub fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Ctx>,
    ) -> AssetsResult<AssetResource<Ctx>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.open_resource_with_ctx(key, ctx),
            Self::Mem { store, .. } => store.open_resource_with_ctx(key, ctx),
        }
    }

    /// Acquire a resource with processing context for an explicit write path.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened.
    pub fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Ctx>,
    ) -> AssetsResult<AssetResource<Ctx>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.acquire_resource_with_ctx(key, ctx),
            Self::Mem { store, .. } => store.acquire_resource_with_ctx(key, ctx),
        }
    }

    /// Inspect the current resource state without creating or mutating it.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the key is invalid or the backend cannot inspect it.
    pub fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.resource_state(key),
            Self::Mem { store, .. } => store.resource_state(key),
        }
    }

    /// Return the asset root identifier.
    #[must_use]
    pub fn asset_root(&self) -> &str {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.asset_root(),
            Self::Mem { store, .. } => store.asset_root(),
        }
    }

    /// Whether this backend is ephemeral (in-memory).
    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Mem { .. })
    }

    /// Compatibility helper for callers that only care about committed resources.
    #[must_use]
    pub fn has_resource(&self, key: &ResourceKey) -> bool {
        matches!(
            self.resource_state(key),
            Ok(AssetResourceState::Committed { .. })
        )
    }

    /// Remove a single resource from the store. Also evicts the key
    /// from the aggregate byte availability index.
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
        self.availability().remove(key);
    }

    /// Return the root directory for the asset store.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.root_dir(),
            Self::Mem { store, .. } => store.root_dir(),
        }
    }

    /// Delete the entire asset directory.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the directory cannot be removed.
    pub fn delete_asset(&self) -> AssetsResult<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk { store, .. } => store.delete_asset(),
            Self::Mem { store, .. } => store.delete_asset(),
        }
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
    /// seeding the aggregate (Phase P-3's observer does the actual
    /// seeding on open).
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

    /// Return `true` when every byte in `range` is already present for
    /// the resource, or when the range is empty.
    ///
    /// Fast path checks the aggregate; slow path falls back to
    /// `resource_state` so pre-existing committed files on disk are
    /// discoverable even before the Phase P-3 observer wires in.
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
}

#[cfg(not(target_arch = "wasm32"))]
impl<Ctx> From<DiskStore<Ctx>> for AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: DiskStore<Ctx>) -> Self {
        Self::Disk {
            store,
            availability: AvailabilityIndex::new(),
        }
    }
}

impl<Ctx> From<MemStore<Ctx>> for AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: MemStore<Ctx>) -> Self {
        Self::Mem {
            store,
            availability: AvailabilityIndex::new(),
        }
    }
}
