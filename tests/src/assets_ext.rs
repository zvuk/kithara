#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use kithara::assets::{AssetStore, StorageBackend};

use crate::bufpool_ext::{TestPools, pools};

#[cfg(not(target_arch = "wasm32"))]
pub fn disk_asset_store(root: impl Into<PathBuf>) -> AssetStore<TestPools> {
    AssetStore::builder(pools())
        .backend(StorageBackend::Disk { root: root.into() })
        .build()
}

pub fn memory_asset_store() -> AssetStore<TestPools> {
    AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .build()
}
