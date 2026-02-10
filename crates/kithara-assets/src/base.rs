#![forbid(unsafe_code)]

use std::{
    fmt::Debug,
    hash::Hash,
    path::{Path, PathBuf},
};

use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource, StorageResource};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{AssetsError, AssetsResult},
    key::ResourceKey,
};

/// Explicit public contract for the assets abstraction.
///
/// `kithara-assets` is about *assets* and their *resources*:
/// - an **asset** is a logical unit that may consist of multiple files/resources,
/// - a **resource** is addressed by [`ResourceKey`] and is opened as a unified
///   resource supporting both streaming and atomic access patterns.
///
/// The `Context` associated type allows decorators to pass additional processing
/// context (e.g. encryption info) when opening resources. Use `()` for no context.
///
/// `IndexRes` is the resource type used for internal index persistence (pins, LRU).
/// Disk-backed stores use `MmapResource`; in-memory stores use `MemResource`.
pub trait Assets: Clone + Send + Sync + 'static {
    /// Type returned by `open_resource`. Must be Clone for caching.
    type Res: kithara_storage::ResourceExt + Clone + Send + Sync + Debug + 'static;
    /// Context type for resource processing. Use `()` for no context.
    type Context: Clone + Send + Sync + Hash + Eq + Debug + 'static;
    /// Resource type for index persistence (pins, LRU).
    type IndexRes: kithara_storage::ResourceExt + Clone + Send + Sync + Debug + 'static;

    /// Open a resource with optional context (main method).
    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res>;

    /// Convenience method - open a resource without context.
    fn open_resource(&self, key: &ResourceKey) -> AssetsResult<Self::Res> {
        self.open_resource_with_ctx(key, None)
    }

    /// Open the resource used for persisting the pins index.
    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Open the resource used for persisting the LRU index.
    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Open the resource used for persisting the coverage index.
    fn open_coverage_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Delete the entire asset (all resources under this store's `asset_root`).
    fn delete_asset(&self) -> AssetsResult<()>;

    /// Remove a single resource by key.
    ///
    /// Default implementation is a no-op (suitable for disk stores where
    /// resources are managed by filesystem eviction). In-memory stores
    /// override this to free memory.
    fn remove_resource(&self, _key: &ResourceKey) -> AssetsResult<()> {
        Ok(())
    }

    /// Return the root directory for disk-backed implementations.
    fn root_dir(&self) -> &Path;

    /// Return the asset root identifier for this store.
    fn asset_root(&self) -> &str;
}

/// Concrete on-disk [`Assets`] implementation for a single asset.
///
/// Maps [`ResourceKey`] to disk paths under a root directory.
/// Each `DiskAssetStore` is scoped to a single `asset_root`.
#[derive(Clone, Debug)]
pub struct DiskAssetStore {
    root_dir: PathBuf,
    asset_root: String,
    cancel: CancellationToken,
}

impl DiskAssetStore {
    /// Create a store rooted at `root_dir` for a specific `asset_root`.
    pub fn new<P: Into<PathBuf>, S: Into<String>>(
        root_dir: P,
        asset_root: S,
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
        match key {
            ResourceKey::Relative(rel) => {
                let asset_root =
                    sanitize_rel(&self.asset_root).map_err(|()| AssetsError::InvalidKey)?;
                let rel_path = sanitize_rel(rel).map_err(|()| AssetsError::InvalidKey)?;
                Ok(self.root_dir.join(asset_root).join(rel_path))
            }
            ResourceKey::Absolute(path) => Ok(path.clone()),
        }
    }

    fn pins_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("pins.bin")
    }

    fn lru_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("lru.bin")
    }

    fn coverage_index_path(&self) -> PathBuf {
        self.root_dir.join("_index").join("cov.bin")
    }

    fn open_storage_resource(
        &self,
        key: &ResourceKey,
        path: PathBuf,
    ) -> AssetsResult<MmapResource> {
        let mode = if key.is_absolute() {
            OpenMode::ReadOnly
        } else {
            OpenMode::Auto
        };
        Ok(Resource::open(
            self.cancel.clone(),
            MmapOptions {
                path,
                initial_len: None,
                mode,
            },
        )?)
    }

    fn open_index_resource(&self, path: PathBuf) -> AssetsResult<MmapResource> {
        Ok(Resource::open(
            self.cancel.clone(),
            MmapOptions {
                path,
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )?)
    }
}

impl Assets for DiskAssetStore {
    type Res = StorageResource;
    type Context = ();
    type IndexRes = MmapResource;

    fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        let path = self.resource_path(key)?;
        let mmap = self.open_storage_resource(key, path)?;
        Ok(StorageResource::Mmap(mmap))
    }

    fn open_pins_index_resource(&self) -> AssetsResult<MmapResource> {
        let path = self.pins_index_path();
        self.open_index_resource(path)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<MmapResource> {
        let path = self.lru_index_path();
        self.open_index_resource(path)
    }

    fn open_coverage_index_resource(&self) -> AssetsResult<MmapResource> {
        let path = self.coverage_index_path();
        self.open_index_resource(path)
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(kithara_storage::StorageError::Cancelled.into());
        }
        delete_asset_dir(&self.root_dir, &self.asset_root).map_err(std::convert::Into::into)
    }
}

/// Delete an asset directory by `asset_root` directly via filesystem.
pub(crate) fn delete_asset_dir(root_dir: &Path, asset_root: &str) -> std::io::Result<()> {
    let safe = sanitize_rel(asset_root).map_err(|()| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid asset_root")
    })?;
    let path = root_dir.join(safe);
    match std::fs::remove_dir_all(&path) {
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
    use kithara_storage::{ResourceExt, ResourceStatus};
    use rstest::rstest;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::key::ResourceKey;

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

    #[expect(clippy::unwrap_used)]
    #[test]
    fn test_open_absolute_resource_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("local_audio.mp3");
        std::fs::write(&file_path, b"fake audio data").unwrap();

        let store = DiskAssetStore::new(dir.path().join("cache"), "_", CancellationToken::new());

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key).unwrap();

        // Should be Committed (read-only file already exists)
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        // Should read existing data
        let mut buf = [0u8; 15];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"fake audio data");
    }
}
