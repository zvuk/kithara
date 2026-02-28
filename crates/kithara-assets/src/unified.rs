#![forbid(unsafe_code)]

//! Unified asset store: disk or memory backend.

use std::{fmt::Debug, hash::Hash, path::Path};

#[cfg(not(target_arch = "wasm32"))]
use crate::store::DiskStore;
use crate::{
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

    /// Check whether a resource is currently in the LRU cache.
    ///
    /// Both Disk and Mem backends use the same `CachedAssets` LRU layer.
    /// Resources can be evicted from either (Mem loses data, Disk loses files
    /// via the `EvictAssets` decorator). Uses LRU peek (no promotion).
    #[must_use]
    pub fn has_resource(&self, key: &ResourceKey) -> bool {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Disk(s) => s.has_resource(key),
            Self::Mem(s) => s.has_resource(key),
        }
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
