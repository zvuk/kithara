#![forbid(unsafe_code)]

//! Storage backend: disk or memory asset store.

use std::{fmt::Debug, hash::Hash};

use crate::{
    base::Assets,
    error::AssetsResult,
    key::ResourceKey,
    store::{AssetResource, AssetStore, MemStore},
};

/// Storage backend: disk or memory asset store.
///
/// Provides a unified interface over `AssetStore` (disk) and `MemStore` (memory).
/// Both variants return the same `AssetResource` type.
///
/// Generic parameter `Ctx` is the processing context type.
/// Use `()` (default) for no processing, or `DecryptContext` for DRM decryption.
#[derive(Clone, Debug)]
pub enum AssetsBackend<Ctx = ()>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    /// File-backed storage with mmap resources.
    Disk(AssetStore<Ctx>),
    /// In-memory storage (ephemeral, no disk artifacts).
    Mem(MemStore<Ctx>),
}

impl<Ctx> AssetsBackend<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    /// Open a resource by key (no processing context).
    pub fn open_resource(&self, key: &ResourceKey) -> AssetsResult<AssetResource<Ctx>> {
        match self {
            Self::Disk(s) => s.open_resource(key),
            Self::Mem(s) => s.open_resource(key),
        }
    }

    /// Open a resource with processing context.
    ///
    /// When `ctx` is `Some`, the resource will be processed on commit
    /// (e.g. AES-128-CBC decryption). When `None`, data passes through unchanged.
    pub fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Ctx>,
    ) -> AssetsResult<AssetResource<Ctx>> {
        match self {
            Self::Disk(s) => s.open_resource_with_ctx(key, ctx),
            Self::Mem(s) => s.open_resource_with_ctx(key, ctx),
        }
    }

    /// Return the asset root identifier.
    pub fn asset_root(&self) -> &str {
        match self {
            Self::Disk(s) => s.asset_root(),
            Self::Mem(s) => s.asset_root(),
        }
    }

    /// Whether this backend is ephemeral (in-memory).
    pub fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Mem(_))
    }

    /// Remove a single resource from the store.
    ///
    /// `MemStore`: removes from `DashMap` through decorator chain.
    /// `DiskStore`: no-op (default impl in `Assets` trait).
    pub fn remove_resource(&self, key: &ResourceKey) {
        match self {
            Self::Disk(s) => {
                let _ = s.remove_resource(key);
            }
            Self::Mem(s) => {
                let _ = s.remove_resource(key);
            }
        }
    }
}

impl<Ctx> From<AssetStore<Ctx>> for AssetsBackend<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: AssetStore<Ctx>) -> Self {
        Self::Disk(store)
    }
}

impl<Ctx> From<MemStore<Ctx>> for AssetsBackend<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn from(store: MemStore<Ctx>) -> Self {
        Self::Mem(store)
    }
}
