#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

/// Key type for addressing resources within an asset.
///
/// A `ResourceKey` identifies a single resource (file) within an asset store.
/// The asset store is already scoped to a specific `asset_root`, so `ResourceKey`
/// only contains the relative path within that asset.
///
/// Higher layers are responsible for choosing `rel_path` (e.g. `media/audio.mp3`,
/// `segments/0001.m4s`).
///
/// The assets store only:
/// - safely maps keys under `<root_dir>/<asset_root>`,
/// - prevents path traversal / absolute paths,
/// - opens the file using the requested resource type (streaming vs atomic).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceKey(String);

impl ResourceKey {
    pub fn new(rel_path: impl Into<String>) -> Self {
        Self(rel_path.into())
    }

    /// Extracts the relative path (last path segment) from a URL.
    pub fn from_url(url: &Url) -> Self {
        let rel_path = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("index")
            .to_string();
        Self(rel_path)
    }

    pub fn rel_path(&self) -> &str {
        &self.0
    }
}

impl From<&Url> for ResourceKey {
    fn from(url: &Url) -> Self {
        ResourceKey::from_url(url)
    }
}

impl From<&str> for ResourceKey {
    fn from(s: &str) -> Self {
        ResourceKey::new(s)
    }
}

impl From<String> for ResourceKey {
    fn from(s: String) -> Self {
        ResourceKey::new(s)
    }
}

/// Generates an asset root identifier from a URL.
///
/// This is typically used with the master playlist URL to create a unique
/// directory name for all resources of an HLS stream.
pub fn asset_root_for_url(url: &Url) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_str().as_bytes());
    let hash_result = hasher.finalize();
    hex::encode(&hash_result[..16])
}
