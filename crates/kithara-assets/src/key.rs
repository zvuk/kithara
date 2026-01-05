#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

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

/// Derives a ResourceKey from a URL: asset_root = first 32 chars of SHA-256 hash, rel_path = last path segment.
impl From<&Url> for ResourceKey {
    fn from(url: &Url) -> Self {
        // Use SHA-256 for stable hashing across runs
        let mut hasher = Sha256::new();
        hasher.update(url.as_str().as_bytes());
        let hash_result = hasher.finalize();
        let asset_root = hex::encode(&hash_result[..16]);
        // Get the last path segment as filename
        let rel_path = url
            .path_segments()
            .and_then(|segments| segments.last())
            .filter(|s| !s.is_empty())
            .unwrap_or("index")
            .to_string();
        Self::new(asset_root, rel_path)
    }
}
