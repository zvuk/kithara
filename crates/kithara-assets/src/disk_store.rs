#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource, StorageResource};
use tokio_util::sync::CancellationToken;

use crate::{
    base::Assets,
    error::{AssetsError, AssetsResult},
    key::ResourceKey,
};

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

    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    #[must_use]
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

    fn supports_evict(&self) -> bool {
        // Local-file mode uses absolute keys with empty asset_root.
        // Disable eviction/index writes for this mode.
        !self.asset_root.is_empty()
    }

    fn supports_lease(&self) -> bool {
        // Local-file mode uses absolute keys with empty asset_root.
        // Disable pin persistence for this mode.
        !self.asset_root.is_empty()
    }

    fn supports_cache(&self) -> bool {
        // Local-file mode uses absolute keys with empty asset_root.
        // Disable decorator cache to avoid pin/evict/index overhead for this mode.
        !self.asset_root.is_empty()
    }

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
        delete_asset_dir(&self.root_dir, &self.asset_root).map_err(Into::into)
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
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[kithara::test]
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

        if is_valid {
            let normalized = result.unwrap();
            assert!(
                !normalized.contains('\\'),
                "Backslashes should be normalized"
            );
        }
    }

    #[kithara::test]
    fn test_open_absolute_resource_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("local_audio.mp3");
        std::fs::write(&file_path, b"fake audio data").unwrap();

        let store = DiskAssetStore::new(dir.path().join("cache"), "_", CancellationToken::new());

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key).unwrap();

        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        let mut buf = [0u8; 15];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"fake audio data");
    }
}
