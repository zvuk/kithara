#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use kithara_storage::{AtomicOptions, AtomicResource, DiskOptions, StreamingResource};
use tokio_util::sync::CancellationToken;

use crate::{cache::Assets, error::CacheResult, key::ResourceKey, lease::LeaseAssets};

/// Concrete on-disk [`Assets`] implementation.
///
/// ## Normative
/// - This type is responsible for mapping [`ResourceKey`] → disk paths under a root directory.
/// - `kithara-assets` crate does not "invent" keys; it only *maps* them.
/// - Path mapping must be safe (no absolute paths, no `..`, no empty segments).
/// - This is not a "cache" by name or responsibility; caching/eviction are higher-level policies.
///
/// Note: this type is intentionally small and dumb: it does not implement pinning.
/// Pinning is provided by the `LeaseAssets` decorator.
#[derive(Clone, Debug)]
pub struct DiskAssetStore {
    root_dir: PathBuf,
}

/// Ready-to-use assets store: `DiskAssetStore` composed with `LeaseAssets` pinning decorator.
///
/// This is a type alias (no new wrapper type).
pub type AssetStore = LeaseAssets<DiskAssetStore>;

/// Constructor for the ready-to-use [`AssetStore`].
///
/// We use a free function (not `AssetStore::new`) because `AssetStore` is a type alias.
pub fn asset_store(root_dir: impl Into<PathBuf>) -> AssetStore {
    AssetStore::new(Arc::new(DiskAssetStore::new(root_dir)))
}

impl DiskAssetStore {
    /// Create a store rooted at `root_dir`.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn resource_path(&self, key: &ResourceKey) -> PathBuf {
        // The user explicitly wants `kithara-assets` to not do "string processing" beyond safety.
        // We therefore *validate* and then join. If validation fails, we fall back to a stable,
        // opaque mapping that still preserves the "no path traversal" invariant.
        //
        // NOTE: if you want strict "reject invalid key" instead of hashing fallback, change this
        // function to return Result<PathBuf> and propagate an error.
        let asset_root =
            sanitize_rel(&key.asset_root).unwrap_or_else(|_| sha256_hex(&key.asset_root));
        let rel_path = sanitize_rel(&key.rel_path).unwrap_or_else(|_| sha256_hex(&key.rel_path));

        self.root_dir.join(asset_root).join(rel_path)
    }

    fn meta_path(&self, key: &ResourceKey) -> PathBuf {
        // Metadata resources are also mapped under the same root.
        // The key is treated identically; the caller chooses the key namespace.
        self.resource_path(key)
    }
}

#[async_trait]
impl Assets for DiskAssetStore {
    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AtomicResource> {
        let path = self.resource_path(key);
        Ok(AtomicResource::open(AtomicOptions { path, cancel }))
    }

    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<StreamingResource> {
        let path = self.resource_path(key);
        let res = StreamingResource::open_disk(DiskOptions {
            path,
            cancel,
            initial_len: None,
        })
        .await?;
        Ok(res)
    }

    async fn open_static_meta_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AtomicResource> {
        // IMPORTANT: the lease decorator must not recurse when persisting meta.
        // This store itself does not perform pinning, so there is no recursion here.
        // Exclusion from pinning is the decorator's job (it simply shouldn't pin meta keys).
        let path = self.meta_path(key);
        Ok(AtomicResource::open(AtomicOptions { path, cancel }))
    }
}

fn sanitize_rel(input: &str) -> Result<String, ()> {
    // Minimal normalization: treat backslashes as separators to avoid Windows traversal surprises.
    let s = input.replace('\\', "/");

    if s.is_empty() || s.starts_with('/') {
        return Err(());
    }

    if s.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(());
    }

    Ok(s)
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let out = hasher.finalize();
    hex::encode(out)
}
