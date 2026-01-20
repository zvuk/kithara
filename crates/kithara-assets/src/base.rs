#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use kithara_storage::{AtomicOptions, AtomicResource, DiskOptions, StreamingResource};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{AssetsError, AssetsResult},
    key::ResourceKey,
};

/// Explicit public contract for the assets abstraction.
///
/// ## What this crate is about (normative)
///
/// `kithara-assets` is about *assets* and their *resources*:
/// - an **asset** is a logical unit that may consist of multiple files/resources
///   (e.g. MP3 single file, or HLS playlist + many segments + keys),
/// - a **resource** is addressed by [`ResourceKey`] and can be opened either as:
///   - **atomic** (small files; read/write atomically),
///   - **streaming** (large files; random access, progressive read/write).
///
/// ## What this crate is NOT about (normative)
///
/// This trait does **not** define:
/// - path layout, directories, filename conventions,
/// - string munging / sanitization / hashing,
/// - direct filesystem operations.
///
/// All disk mapping must be encapsulated in the concrete implementation behind this trait.
///
/// ## Leasing / pinning (normative)
///
/// Leasing / pinning is implemented strictly as a decorator (`LeaseAssets`) over a base [`Assets`]
/// implementation. The base `Assets` does not know about pins.
///
/// ## Associated types
///
/// The `StreamingRes` and `AtomicRes` associated types allow decorators to wrap resources.
/// For example, `LeaseAssets` returns `AssetResource<R, LeaseGuard>` instead of raw `R`.
#[async_trait]
pub trait Assets: Clone + Send + Sync + 'static {
    /// Type returned by `open_streaming_resource`. Must be Clone for caching.
    type StreamingRes: Clone + Send + Sync + 'static;
    /// Type returned by `open_atomic_resource`. Must be Clone for caching.
    type AtomicRes: Clone + Send + Sync + 'static;

    /// Open an atomic resource (small object) addressed by `key`.
    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<Self::AtomicRes>;

    /// Open a streaming resource (large object) addressed by `key`.
    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<Self::StreamingRes>;

    /// Open the atomic resource used for persisting the pins index.
    ///
    /// Normative requirements:
    /// - must be a small atomic file-like resource,
    /// - must be excluded from pinning (otherwise the lease decorator will recurse),
    /// - must be stable for the lifetime of this assets instance.
    ///
    /// Key selection (where this lives on disk) is an implementation detail of the concrete
    /// `Assets` implementation. Higher layers must not construct or assume a `ResourceKey` here.
    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource>;

    /// Delete the entire asset (all resources under this store's `asset_root`).
    ///
    /// ## Normative
    /// - This is used by eviction/GC decorators (LRU, quota enforcement).
    /// - Base `Assets` implementations must NOT apply pin/lease semantics here.
    /// - Higher layers must ensure pinned assets are not deleted (decorators enforce this).
    async fn delete_asset(&self) -> AssetsResult<()>;

    /// Open the atomic resource used for persisting the LRU index.
    ///
    /// ## Normative
    /// - must be a small atomic file-like resource,
    /// - must be stable for the lifetime of this assets instance.
    ///
    /// Key selection (where this lives on disk) is an implementation detail of the concrete
    /// `Assets` implementation.
    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource>;

    /// Return the root directory for disk-backed implementations.
    ///
    /// For disk-backed implementations, this returns the root directory path.
    /// For in-memory or network-backed implementations, this may return a placeholder path.
    fn root_dir(&self) -> &Path;

    /// Return the asset root identifier for this store.
    ///
    /// Each store is scoped to a single asset, identified by this string
    /// (typically a hash of the master playlist URL).
    fn asset_root(&self) -> &str;
}

/// Concrete on-disk [`Assets`] implementation for a single asset.
///
/// ## Normative
/// - This type is responsible for mapping [`ResourceKey`] → disk paths under a root directory.
/// - Each `DiskAssetStore` is scoped to a single `asset_root` (e.g. hash of master playlist URL).
/// - `kithara-assets` crate does not "invent" keys; it only *maps* them.
/// - Path mapping must be safe (no absolute paths, no `..`, no empty segments).
/// - This is not a "cache" by name or responsibility; caching/eviction are higher-level policies.
///
/// Note: this type is intentionally small and dumb: it does not implement pinning or eviction.
/// Pinning is provided by the `LeaseAssets` decorator.
/// Eviction is provided by the `EvictAssets` decorator.
/// Caching is provided by the `CachedAssets` decorator.
#[derive(Clone, Debug)]
pub struct DiskAssetStore {
    root_dir: PathBuf,
    asset_root: String,
    cancel: CancellationToken,
}

impl DiskAssetStore {
    /// Create a store rooted at `root_dir` for a specific `asset_root`.
    pub fn new(
        root_dir: impl Into<PathBuf>,
        asset_root: impl Into<String>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            root_dir: root_dir.into(),
            asset_root: asset_root.into(),
            cancel,
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn resource_path(&self, key: &ResourceKey) -> AssetsResult<PathBuf> {
        let asset_root = sanitize_rel(&self.asset_root).map_err(|()| AssetsError::InvalidKey)?;
        let rel_path = sanitize_rel(key.rel_path()).map_err(|()| AssetsError::InvalidKey)?;
        Ok(self.root_dir.join(asset_root).join(rel_path))
    }

    fn pins_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("pins.json")
    }

    fn lru_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("lru.json")
    }
}

#[async_trait]
impl Assets for DiskAssetStore {
    type StreamingRes = StreamingResource;
    type AtomicRes = AtomicResource;

    fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<Self::AtomicRes> {
        let path = self.resource_path(key)?;
        Ok(AtomicResource::open(AtomicOptions {
            path,
            cancel: self.cancel.clone(),
        }))
    }

    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<Self::StreamingRes> {
        let path = self.resource_path(key)?;
        let res = StreamingResource::open_disk(DiskOptions {
            path,
            cancel: self.cancel.clone(),
            initial_len: None,
        })
        .await?;
        Ok(res)
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        let path = self.pins_index_path();
        Ok(AtomicResource::open(AtomicOptions {
            path,
            cancel: self.cancel.clone(),
        }))
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(kithara_storage::StorageError::Cancelled.into());
        }
        delete_asset_dir(&self.root_dir, &self.asset_root)
            .await
            .map_err(|e| e.into())
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        let path = self.lru_index_path();
        Ok(AtomicResource::open(AtomicOptions {
            path,
            cancel: self.cancel.clone(),
        }))
    }
}

/// Delete an asset directory by `asset_root` directly via filesystem.
///
/// This is a low-level operation used by eviction to delete other assets.
/// It does NOT clear any in-memory caches (the caller is responsible for that if needed).
pub(crate) async fn delete_asset_dir(root_dir: &Path, asset_root: &str) -> std::io::Result<()> {
    let safe = sanitize_rel(asset_root).map_err(|()| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid asset_root")
    })?;
    let path = root_dir.join(safe);
    match tokio::fs::remove_dir_all(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub(crate) fn sanitize_rel(input: &str) -> Result<String, ()> {
    let s = input.replace('\\', "/");
    if s.is_empty() || s.starts_with('/') || s.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(());
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("valid.txt", true, "Simple filename")]
    #[case("dir/valid.txt", true, "Nested path")]
    #[case("a/b/c/file.mp3", true, "Multiple levels")]
    #[case("audio-file_123.m4a", true, "Filename with special chars")]
    #[case("/absolute", false, "Absolute path (leading slash)")]
    #[case("../traversal", false, "Dotdot traversal at start")]
    #[case("dir/../file", false, "Dotdot traversal in middle")]
    #[case("a/b/../c", false, "Dotdot traversal")]
    #[case("", false, "Empty string")]
    #[case("dir//file", false, "Double slash (empty component)")]
    #[case("dir/", false, "Trailing slash (empty component)")]
    #[case("/", false, "Single slash")]
    #[case(".", true, "Current directory reference")]
    #[case("dir/./file.txt", true, "Dot component (allowed)")]
    #[case("windows\\path", true, "Windows backslash (gets normalized)")]
    #[case("dir\\file.txt", true, "Mixed slashes")]
    fn test_path_validation(
        #[case] path: &str,
        #[case] is_valid: bool,
        #[case] _description: &str,
    ) {
        let result = sanitize_rel(path);
        assert_eq!(result.is_ok(), is_valid, "Path: {:?}", path);

        // If valid, check normalization worked
        if is_valid {
            let normalized = result.unwrap();
            assert!(
                !normalized.contains('\\'),
                "Backslashes should be normalized"
            );
        }
    }
}
