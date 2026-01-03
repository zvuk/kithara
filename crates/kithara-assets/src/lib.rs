#![forbid(unsafe_code)]

//! # kithara-assets
//!
//! Persistent disk assets store for Kithara.
//!
//! ## Public contract
//!
//! The explicit public contract is the [`Assets`] trait.
//! Everything else should be considered an implementation detail.
//!
//! ## Key mapping (normative)
//!
//! Resources are addressed by strings chosen by higher layers:
//! - `asset_root`: e.g. hex(AssetId) / ResourceHash
//! - `rel_path`: e.g. `media/audio.mp3`, `segments/0001.m4s`
//!
//! Disk mapping is:
//! - `<cache_root>/<asset_root>/<rel_path>`
//!
//! Assets does not “invent” paths; it only enforces safety (no absolute paths, no `..`, no empty
//! segments).
//!
//! ## Auto-pin (lease) semantics
//!
//! All resources opened through [`Assets`] are automatically pinned by `asset_root` for the lifetime
//! of the returned handle. This prevents eviction of the active asset while any of its resources are
//! still in use.
//!
//! The pin is expressed as an RAII guard stored inside [`AssetResource`]. Drop the handle to release
//! the pin.
//!
//! ## Global index (best-effort)
//!
//! `_index/state.json` is a small, atomic file (temp → rename) used as best-effort metadata.
//! Filesystem remains the source of truth; the index may be missing and can be rebuilt later.

mod lease;
mod resource;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use kithara_storage::{
    AtomicOptions, AtomicResource, DiskOptions, Resource, StorageError, StreamingResource,
};
use thiserror::Error;

use crate::lease::LeaseTable;

/// Key type for addressing resources.
///
/// The assets store does not construct these strings. Higher layers are responsible for:
/// - choosing `asset_root` (e.g. hex(AssetId) / ResourceHash),
/// - choosing `rel_path` (e.g. `media/audio.mp3`, `segments/0001.m4s`).
///
/// The assets store only:
/// - safely maps them under `root_dir`,
/// - prevents path traversal / absolute paths,
/// - opens the file using the requested resource type (streaming vs atomic).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceKey {
    pub asset_root: String,
    pub rel_path: String,
}

impl ResourceKey {
    pub fn new(asset_root: impl Into<String>, rel_path: impl Into<String>) -> Self {
        Self {
            asset_root: asset_root.into(),
            rel_path: rel_path.into(),
        }
    }
}

/// Options for opening the assets store.
#[derive(Clone, Debug)]
pub struct CacheOptions {
    /// Root directory for all on-disk assets.
    pub root_dir: PathBuf,
}

impl CacheOptions {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }
}

pub use crate::resource::AssetResource;

/// Explicit public contract for the assets store.
///
/// Everything not represented here should be treated as an implementation detail (non-`pub`).
#[async_trait]
pub trait Assets: Send + Sync + 'static {
    /// Root directory of this assets instance.
    fn root_dir(&self) -> &Path;

    /// Open the on-disk index resource (`asset_root="_index" / rel_path="state.json"`) as an atomic resource.
    async fn open_index_resource(&self, cancel: CancellationToken)
    -> AssetResource<AtomicResource>;

    /// Load the global index from `asset_root="_index" / rel_path="state.json"`.
    ///
    /// If the file is missing, returns a default empty index.
    async fn load_index(&self, cancel: CancellationToken) -> CacheResult<AssetIndex>;

    /// Store the global index to `asset_root="_index" / rel_path="state.json"` atomically.
    async fn store_index(&self, cancel: CancellationToken, index: &AssetIndex) -> CacheResult<()>;

    /// Open a streaming resource by key (large objects).
    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<StreamingResource>>;

    /// Open an atomic resource by key (small objects: playlists, keys, metadata).
    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> AssetResource<AtomicResource>;
}

/// Concrete implementation of [`Assets`].
#[derive(Clone)]
pub struct AssetCache {
    root_dir: PathBuf,
    leases: Arc<LeaseTable>,
}

impl fmt::Debug for AssetCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssetCache")
            .field("root_dir", &self.root_dir)
            .finish_non_exhaustive()
    }
}

impl AssetCache {
    /// Open (or create) the assets store.
    pub fn open(opts: CacheOptions) -> CacheResult<Self> {
        Ok(Self {
            root_dir: opts.root_dir,
            leases: Arc::new(LeaseTable::default()),
        })
    }
}

#[async_trait]
impl Assets for AssetCache {
    fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    async fn open_index_resource(
        &self,
        cancel: CancellationToken,
    ) -> AssetResource<AtomicResource> {
        self.open_atomic_resource(&ResourceKey::new("_index", "state.json"), cancel)
            .await
    }

    async fn load_index(&self, cancel: CancellationToken) -> CacheResult<AssetIndex> {
        let res = self.open_index_resource(cancel).await;
        let bytes = res.read().await?;

        if bytes.is_empty() {
            return Ok(AssetIndex::default());
        }

        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn store_index(&self, cancel: CancellationToken, index: &AssetIndex) -> CacheResult<()> {
        let res = self.open_index_resource(cancel).await;
        let bytes = serde_json::to_vec_pretty(index)?;
        res.write(&bytes).await?;
        Ok(())
    }

    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<StreamingResource>> {
        let path = resource_path(&self.root_dir, key);

        let opts = DiskOptions {
            path,
            cancel,
            initial_len: None,
        };

        let res = StreamingResource::open_disk(opts).await?;
        let lease = self.leases.pin(&key.asset_root).await;
        Ok(AssetResource::new(res, lease))
    }

    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<AtomicResource>> {
        let path = resource_path(&self.root_dir, key);
        let res = AtomicResource::open(AtomicOptions { path, cancel });

        let lease = self.leases.pin(&key.asset_root).await;
        Ok(AssetResource::new(res, lease))
    }
}

fn resource_path(root_dir: &Path, key: &ResourceKey) -> PathBuf {
    let asset_root = sanitize_rel(&key.asset_root).unwrap_or_else(|_| sha256_hex(&key.asset_root));
    let rel_path = sanitize_rel(&key.rel_path).unwrap_or_else(|_| sha256_hex(&key.rel_path));
    root_dir.join(asset_root).join(rel_path)
}

/// Assets store errors.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

pub type CacheResult<T> = Result<T, CacheError>;

/// Global index data (skeleton).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AssetIndex {
    pub version: u32,
    pub resources: Vec<AssetIndexEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetIndexEntry {
    pub rel_path: String,
    pub status: ResourceStatus,
    pub final_len: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResourceStatus {
    InProgress,
    Ready,
    Failed,
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let out = hasher.finalize();
    hex::encode(out)
}

fn sanitize_rel(input: &str) -> Result<String, ()> {
    let s = input.replace('\\', "/");
    if s.starts_with('/') || s.is_empty() {
        return Err(());
    }
    if s.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(());
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "tests will be rewritten for the new resource-based API"]
    fn placeholder() {
        // Intentionally empty.
    }
}
