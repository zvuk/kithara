#![forbid(unsafe_code)]

use kithara_core::{AssetId, CoreError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("not implemented")]
    Unimplemented,

    #[error("core error: {0}")]
    Core(#[from] CoreError),
}

pub type CacheResult<T> = Result<T, CacheError>;

#[derive(Clone, Debug)]
pub struct CacheOptions {
    pub max_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct AssetCache;

impl AssetCache {
    pub fn open(_opts: CacheOptions) -> CacheResult<Self> {
        unimplemented!("kithara-cache: AssetCache::open is not implemented yet")
    }

    pub fn asset(&self, _asset: AssetId) -> AssetHandle {
        unimplemented!("kithara-cache: AssetCache::asset is not implemented yet")
    }
}

#[derive(Clone, Debug)]
pub struct AssetHandle;

impl AssetHandle {
    pub fn exists(&self, _rel_path: &str) -> bool {
        unimplemented!("kithara-cache: AssetHandle::exists is not implemented yet")
    }
}
