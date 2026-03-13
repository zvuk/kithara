#![forbid(unsafe_code)]

//! Unified asset store: disk or memory backend.

use std::{fmt::Debug, hash::Hash, path::Path};

#[cfg(not(target_arch = "wasm32"))]
use crate::store::DiskStore;
use crate::{
    AssetResourceState,
    base::Assets,
    error::AssetsResult,
    key::ResourceKey,
    store::{AssetResource, MemStore},
};

/// Unified storage backend for assets.
///
/// Dispatches all operations to an inner disk or memory store chain.
#[derive(Clone, Debug)]
pub enum AssetStore<Ctx = ()>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    /// File-backed storage with mmap resources.
    #[cfg(not(target_arch = "wasm32"))]
    Disk(DiskStore<Ctx>),
    /// In-memory storage (ephemeral, no disk artifacts).
    Mem(MemStore<Ctx>),
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
            Self::Disk(s) => s.open_resource(key),
            Self::Mem(s) => s.open_resource(key),
        }
    }

    /// Acquire a resource explicitly for mutation.
    ///
    /// This is an alias for callers that want to make write intent explicit
    /// at the call site.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened.
    pub fn acquire_resource(&self, key: &ResourceKey) -> AssetsResult<AssetResource<Ctx>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk(s) => s.acquire_resource(key),
            Self::Mem(s) => s.acquire_resource(key),
        }
    }

    /// Open a resource with processing context.
    ///
    /// When `ctx` is `Some`, the resource will be processed on commit
    /// (e.g. AES-128-CBC decryption). When `None`, data passes through unchanged.
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
            Self::Disk(s) => s.open_resource_with_ctx(key, ctx),
            Self::Mem(s) => s.open_resource_with_ctx(key, ctx),
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
            Self::Disk(s) => s.acquire_resource_with_ctx(key, ctx),
            Self::Mem(s) => s.acquire_resource_with_ctx(key, ctx),
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
            Self::Disk(s) => s.resource_state(key),
            Self::Mem(s) => s.resource_state(key),
        }
    }

    /// Return the asset root identifier.
    #[must_use]
    pub fn asset_root(&self) -> &str {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk(s) => s.asset_root(),
            Self::Mem(s) => s.asset_root(),
        }
    }

    /// Whether this backend is ephemeral (in-memory).
    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Mem(_))
    }

    /// Compatibility helper for callers that only care about committed resources.
    #[must_use]
    pub fn has_resource(&self, key: &ResourceKey) -> bool {
        matches!(
            self.resource_state(key),
            Ok(AssetResourceState::Committed { .. })
        )
    }

    /// Remove a single resource from the store.
    pub fn remove_resource(&self, key: &ResourceKey) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk(s) => {
                let _ = s.remove_resource(key);
            }
            Self::Mem(s) => {
                let _ = s.remove_resource(key);
            }
        }
    }

    /// Return the root directory for the asset store.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk(s) => s.root_dir(),
            Self::Mem(s) => s.root_dir(),
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
            Self::Disk(s) => s.delete_asset(),
            Self::Mem(s) => s.delete_asset(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<Ctx> From<DiskStore<Ctx>> for AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: DiskStore<Ctx>) -> Self {
        Self::Disk(store)
    }
}

impl<Ctx> From<MemStore<Ctx>> for AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: MemStore<Ctx>) -> Self {
        Self::Mem(store)
    }
}
