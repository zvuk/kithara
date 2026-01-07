#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

/// Key type for addressing resources.
///
/// The assets store does not construct these strings. Higher layers are responsible for:
/// - choosing `asset_root` (e.g. hex(AssetId)),
/// - choosing `rel_path` (e.g. `media/audio.mp3`, `segments/0001.m4s`).
///
/// The assets store only:
/// - safely maps them under `root_dir`,
/// - prevents path traversal / absolute paths,
/// - opens the file using the requested resource type (streaming vs atomic).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceKey {
    pub(crate) asset_root: String,
    pub(crate) rel_path: String,
}

impl ResourceKey {
    pub fn new(asset_root: impl Into<String>, rel_path: impl Into<String>) -> Self {
        Self {
            asset_root: asset_root.into(),
            rel_path: rel_path.into(),
        }
    }

    pub fn asset_root_for_url(url: &Url) -> String {
        let mut hasher = Sha256::new();
        hasher.update(url.as_str().as_bytes());
        let hash_result = hasher.finalize();
        hex::encode(&hash_result[..16])
    }

    pub fn from_url_with_asset_root(asset_root: impl Into<String>, url: &Url) -> Self {
        let rel_path = url
            .path_segments()
            .and_then(|segments| segments.last())
            .filter(|s| !s.is_empty())
            .unwrap_or("index")
            .to_string();
        Self::new(asset_root, rel_path)
    }

    pub fn asset_root(&self) -> &str {
        &self.asset_root
    }

    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }
}

/// Derives a ResourceKey from a URL using its own hash as asset_root.
impl From<&Url> for ResourceKey {
    fn from(url: &Url) -> Self {
        let asset_root = ResourceKey::asset_root_for_url(url);
        ResourceKey::from_url_with_asset_root(asset_root, url)
    }
}
