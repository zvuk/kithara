use crate::CachePath;
use kithara_core::AssetId;
use std::path::PathBuf;

/// Filesystem layout utilities for cache directory structure.
/// Provides asset directory hashing and path construction utilities.
pub struct FsLayout;

impl FsLayout {
    /// Create the asset directory structure based on AssetId hash
    pub fn asset_dir(root_dir: &std::path::Path, asset_id: AssetId) -> PathBuf {
        let asset_key = hex::encode(asset_id.as_bytes());
        root_dir.join(&asset_key[0..2]).join(&asset_key[2..4])
    }

    /// Create temporary file path for atomic writes
    pub fn temp_file(
        root_dir: &std::path::Path,
        asset_id: AssetId,
        rel_path: &CachePath,
    ) -> PathBuf {
        let asset_dir = Self::asset_dir(root_dir, asset_id);
        asset_dir.join(format!("{}.tmp", rel_path.as_string()))
    }

    /// Create final file path for completed writes
    pub fn final_file(
        root_dir: &std::path::Path,
        asset_id: AssetId,
        rel_path: &CachePath,
    ) -> PathBuf {
        let asset_dir = Self::asset_dir(root_dir, asset_id);
        asset_dir.join(rel_path.as_path_buf())
    }
}
