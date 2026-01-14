#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::{AssetsError, AssetsResult};

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

/// Unique identifier for an asset, derived from a URL.
///
/// `AssetId` is computed by hashing the canonicalized URL. Two URLs that differ
/// only in case, default ports, query parameters, or fragments will produce
/// the same `AssetId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssetId([u8; 32]);

impl AssetId {
    pub fn from_url(url: &Url) -> AssetsResult<AssetId> {
        let canonical = canonicalize_for_asset(url)?;
        let hash = Sha256::digest(canonical.as_bytes());
        Ok(AssetId(hash.into()))
    }
}

/// Generates an asset root identifier from a URL.
///
/// This is typically used with the master playlist URL to create a unique
/// directory name for all resources of an HLS stream.
///
/// Uses the same canonicalization as `AssetId` to ensure consistency:
/// URLs that differ only in case, default ports, or query/fragment
/// will produce the same asset root.
pub fn asset_root_for_url(url: &Url) -> String {
    // Use canonical form for consistent hashing.
    // Fall back to raw URL if canonicalization fails (e.g., file:// URLs).
    let canonical = canonicalize_for_asset(url).unwrap_or_else(|_| url.as_str().to_string());

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let hash_result = hasher.finalize();
    hex::encode(&hash_result[..16])
}

/// Canonicalizes a URL for asset identification.
///
/// This function normalizes URLs to ensure consistent identification:
/// - Removes query parameters and fragments
/// - Normalizes scheme and host to lowercase
/// - Removes default ports (80 for HTTP, 443 for HTTPS)
pub fn canonicalize_for_asset(url: &Url) -> AssetsResult<String> {
    // Validate that URL has required components for asset identification
    if url.scheme().is_empty() {
        return Err(AssetsError::MissingComponent("scheme".to_string()));
    }
    if url.host().is_none() {
        return Err(AssetsError::MissingComponent("host".to_string()));
    }

    let mut canonical = url.clone();

    // Remove fragment and query
    canonical.set_fragment(None);
    canonical.set_query(None);

    // Normalize scheme and host to lowercase
    let scheme = canonical.scheme();
    let scheme_lower = scheme.to_lowercase();
    if scheme != scheme_lower {
        let _ = canonical.set_scheme(&scheme_lower);
    }

    if let Some(host) = canonical.host_str() {
        let host_lower = host.to_lowercase();
        if host != host_lower {
            let _ = canonical.set_host(Some(&host_lower));
        }
    }

    // Remove default ports
    match (canonical.scheme(), canonical.port()) {
        ("https", Some(443)) | ("http", Some(80)) => {
            let _ = canonical.set_port(None);
        }
        _ => {}
    }

    canonical
        .to_string()
        .parse::<String>()
        .map_err(|e| AssetsError::Canonicalization(e.to_string()))
}
