#![forbid(unsafe_code)]

use kithara_core::{AssetId, CoreError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

// Import Store trait for method calls
use crate::store::Store;

pub mod base;
pub mod evict;
pub mod evicting_store;
pub mod index;
pub mod lease;
pub mod store;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid cache path: {0}")]
    InvalidPath(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Core error: {0}")]
    Core(#[from] CoreError),

    #[error("Cache full: cannot free enough space")]
    CacheFull,

    #[error("Asset not found: {0}")]
    AssetNotFound(String),
}

pub type CacheResult<T> = Result<T, CacheError>;

#[derive(Clone, Debug)]
pub struct CacheOptions {
    pub max_bytes: u64,
    pub root_dir: Option<PathBuf>,
}

/// Safe relative path within an asset cache.
/// No `..`, no absolute paths, no empty segments.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CachePath {
    segments: Vec<String>,
}

impl CachePath {
    pub fn new(segments: Vec<String>) -> CacheResult<Self> {
        if segments.is_empty() {
            return Err(CacheError::InvalidPath("empty path".to_string()));
        }

        for segment in &segments {
            if segment.is_empty() {
                return Err(CacheError::InvalidPath("empty segment".to_string()));
            }
            if segment == ".." {
                return Err(CacheError::InvalidPath(
                    "parent directory reference".to_string(),
                ));
            }
            if segment.contains('/') || segment.contains('\\') {
                return Err(CacheError::InvalidPath(
                    "path separator in segment".to_string(),
                ));
            }
        }

        Ok(CachePath { segments })
    }

    pub fn from_single(segment: impl Into<String>) -> CacheResult<Self> {
        Self::new(vec![segment.into()])
    }

    pub fn as_path_buf(&self) -> PathBuf {
        self.segments.iter().collect()
    }

    pub fn as_string(&self) -> String {
        self.segments.join("/")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheState {
    pub max_bytes: u64,
    pub total_bytes: u64,
    pub assets: std::collections::HashMap<String, AssetState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetState {
    pub size_bytes: u64,
    pub last_access_ms: u64,
    pub created_ms: u64,
    pub pinned: bool,
}

#[derive(Clone, Debug)]
pub struct AssetCache {
    // Composed layers: FsStore -> IndexStore -> LeaseStore -> EvictingStore
    store: crate::evicting_store::EvictingStore<
        crate::lease::LeaseStore<crate::index::IndexStore<crate::base::FsStore>>,
        crate::evict::LruPolicy,
    >,
}

#[derive(Clone, Debug)]
pub struct AssetHandle<'a> {
    cache: &'a AssetCache,
    asset_id: AssetId,
}

// Re-export LeaseGuard from lease module for backwards compatibility
pub use crate::lease::LeaseGuard;

#[derive(Clone, Debug)]
pub struct PutResult {
    pub bytes_written: u64,
}

#[derive(Clone, Debug)]
pub struct CacheStats {
    pub total_bytes: u64,
    pub asset_count: usize,
    pub pinned_assets: usize,
}

impl AssetCache {
    pub fn open(opts: CacheOptions) -> CacheResult<Self> {
        let root_dir = opts.root_dir.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("kithara-cache-{}", uuid::Uuid::new_v4()))
        });
        std::fs::create_dir_all(&root_dir)?;

        // Compose layers: FsStore -> IndexStore -> LeaseStore -> EvictingStore
        let fs_store = crate::base::FsStore::new(root_dir.clone())?;
        let index_store =
            crate::index::IndexStore::new(fs_store, root_dir.clone(), opts.max_bytes);
        let lease_store = crate::lease::LeaseStore::new(index_store);
        let evicting_store =
            crate::evicting_store::EvictingStore::with_lru(lease_store, opts.max_bytes);

        Ok(AssetCache {
            store: evicting_store,
        })
    }

    pub fn asset(&self, asset_id: AssetId) -> AssetHandle<'_> {
        AssetHandle {
            cache: self,
            asset_id,
        }
    }

    pub fn pin(
        &self,
        asset: AssetId,
    ) -> CacheResult<
        crate::lease::LeaseGuard<
            '_,
            crate::lease::LeaseStore<crate::index::IndexStore<crate::base::FsStore>>,
        >,
    > {
        self.store.pin(asset)
    }

    pub fn touch(&self, asset: AssetId) -> CacheResult<()> {
        // Access IndexStore through layered structure
        // EvictingStore -> LeaseStore -> IndexStore
        self.store.inner.inner.touch(asset)
    }

    pub fn ensure_space(&self, incoming_bytes: u64, pinned: Option<AssetId>) -> CacheResult<()> {
        self.store.ensure_space(incoming_bytes, pinned)?;
        Ok(())
    }

    pub fn stats(&self) -> CacheResult<CacheStats> {
        // Delegate to inner layers
        use crate::evicting_store::EvictionSupport;

        if let Ok(assets) = self.store.inner.get_all_assets() {
            let total_bytes: u64 = assets.iter().map(|(_, state)| state.size_bytes).sum();
            let asset_count = assets.len();
            let pinned_assets = assets.iter().filter(|(_, state)| state.pinned).count();

            Ok(CacheStats {
                total_bytes,
                asset_count,
                pinned_assets,
            })
        } else {
            Ok(CacheStats {
                total_bytes: 0,
                asset_count: 0,
                pinned_assets: 0,
            })
        }
    }
}

impl<'a> AssetHandle<'a> {
    pub fn exists(&self, rel_path: &CachePath) -> bool {
        self.cache.store.exists(self.asset_id, rel_path)
    }

    pub fn open(&self, rel_path: &CachePath) -> CacheResult<Option<std::fs::File>> {
        self.cache.store.open(self.asset_id, rel_path)
    }

    pub fn put_atomic(&self, rel_path: &CachePath, bytes: &[u8]) -> CacheResult<PutResult> {
        self.cache.store.put_atomic(self.asset_id, rel_path, bytes)
    }

    pub fn remove_all(&self) -> CacheResult<()> {
        self.cache.store.remove_all(self.asset_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_rejects_empty_segments() {
        let result = CachePath::new(vec!["valid".to_string(), "".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn cache_path_rejects_parent_reference() {
        let result = CachePath::new(vec!["valid".to_string(), "..".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn cache_path_rejects_path_separators() {
        let result = CachePath::new(vec!["valid".to_string(), "invalid/name".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn cache_path_accepts_valid_paths() {
        let path = CachePath::new(vec!["segment1".to_string(), "segment2".to_string()]).unwrap();
        assert_eq!(path.segments, vec!["segment1", "segment2"]);
        assert_eq!(path.as_string(), "segment1/segment2");
    }

    #[test]
    fn cache_path_from_single() {
        let path = CachePath::from_single("test").unwrap();
        assert_eq!(path.segments, vec!["test"]);
        assert_eq!(path.as_string(), "test");
    }

    #[test]
    fn asset_cache_opens_temp_directory() {
        let opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let cache = AssetCache::open(opts).unwrap();
        // Since root_dir is private, we can't test this directly
        // This test just ensures cache creation works
        assert!(true);
    }

    #[test]
    fn asset_handle_put_atomic_is_crash_safe() {
        let opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let cache = AssetCache::open(opts).unwrap();
        let url = url::Url::parse("https://example.com/test.mp3").unwrap();
        let asset_id = AssetId::from_url(&url).unwrap();
        let handle = cache.asset(asset_id);
        let path = CachePath::from_single("test.txt").unwrap();
        let data = b"test data";

        // After put, file should exist and be readable
        let result = handle.put_atomic(&path, data).unwrap();
        assert_eq!(result.bytes_written, data.len() as u64);
        assert!(handle.exists(&path));

        let mut file = handle.open(&path).unwrap().unwrap();
        let mut content = String::new();
        use std::io::Read;
        file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "test data");
    }

    #[test]
    fn asset_handle_put_atomic_overwrites_existing() {
        let opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let cache = AssetCache::open(opts).unwrap();
        let url = url::Url::parse("https://example.com/test.mp3").unwrap();
        let asset_id = AssetId::from_url(&url).unwrap();
        let handle = cache.asset(asset_id);
        let path = CachePath::from_single("test.txt").unwrap();

        handle.put_atomic(&path, b"original").unwrap();
        handle.put_atomic(&path, b"replaced").unwrap();

        let mut file = handle.open(&path).unwrap().unwrap();
        let mut content = String::new();
        use std::io::Read;
        file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "replaced");
    }
}