#![forbid(unsafe_code)]

//! Storage backend: disk or memory asset store.

use kithara_assets::{AssetResource, AssetStore, Assets, AssetsResult, MemStore, ResourceKey};

/// Storage backend: disk or memory asset store.
///
/// Provides a unified interface over `AssetStore` (disk) and `MemStore` (memory).
/// Both variants return the same `AssetResource` type.
#[derive(Clone, Debug)]
pub enum AssetsBackend {
    /// File-backed storage with mmap resources.
    Disk(AssetStore),
    /// In-memory storage (ephemeral, no disk artifacts).
    Mem(MemStore),
}

impl AssetsBackend {
    /// Open a resource by key.
    pub(crate) fn open_resource(&self, key: &ResourceKey) -> AssetsResult<AssetResource> {
        match self {
            Self::Disk(s) => s.open_resource(key),
            Self::Mem(s) => s.open_resource(key),
        }
    }

    /// Return the asset root identifier.
    pub(crate) fn asset_root(&self) -> &str {
        match self {
            Self::Disk(s) => s.asset_root(),
            Self::Mem(s) => s.asset_root(),
        }
    }

    /// Whether this backend is ephemeral (in-memory).
    pub(crate) fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Mem(_))
    }

    /// Remove a single resource from the store.
    ///
    /// `MemStore`: removes from `DashMap` through decorator chain.
    /// `DiskStore`: no-op (default impl in `Assets` trait).
    pub(crate) fn remove_resource(&self, key: &ResourceKey) {
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
