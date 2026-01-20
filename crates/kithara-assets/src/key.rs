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

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use url::Url;

    use super::*;
    use crate::{AssetsError, canonicalize_for_asset};

    #[rstest]
    #[case(
        "https://example.com/audio.mp3?token=123&quality=high#section",
        "https://example.com/audio.mp3?different=456#other",
        true,
        "AssetId should ignore both query and fragment"
    )]
    #[case(
        "https://example.com/audio.mp3?token=123&quality=high#section",
        "https://example.com/audio.mp3",
        true,
        "AssetId should ignore query and fragment compared to clean URL"
    )]
    #[case(
        "HTTPS://EXAMPLE.COM/audio.mp3",
        "https://example.com/audio.mp3",
        true,
        "AssetId should normalize scheme and host to lowercase"
    )]
    #[case(
        "https://example.com:443/audio.mp3",
        "https://example.com/audio.mp3",
        true,
        "AssetId should remove default HTTPS port 443"
    )]
    #[case(
        "http://example.com:80/audio.mp3",
        "http://example.com/audio.mp3",
        true,
        "AssetId should remove default HTTP port 80"
    )]
    #[case(
        "https://example.com:8443/audio.mp3",
        "https://example.com/audio.mp3",
        false,
        "AssetId should preserve explicit non-default port"
    )]
    fn test_asset_id_comparisons(
        #[case] url1: &str,
        #[case] url2: &str,
        #[case] should_be_equal: bool,
        #[case] description: &str,
    ) {
        let url1 = Url::parse(url1).unwrap();
        let url2 = Url::parse(url2).unwrap();

        let asset1 = AssetId::from_url(&url1).unwrap();
        let asset2 = AssetId::from_url(&url2).unwrap();

        if should_be_equal {
            assert_eq!(asset1, asset2, "{}", description);
        } else {
            assert_ne!(asset1, asset2, "{}", description);
        }
    }

    #[test]
    fn test_asset_id_stable_across_calls() {
        let url = Url::parse("https://example.com/path/to/audio.mp3?version=1.2").unwrap();

        let asset1 = AssetId::from_url(&url).unwrap();
        let asset2 = AssetId::from_url(&url).unwrap();

        assert_eq!(
            asset1, asset2,
            "AssetId should be stable across multiple calls"
        );
    }

    #[rstest]
    #[case("file:///path/to/audio.mp3", "URL without host should error")]
    #[case("", "Empty URL should error")]
    fn test_asset_id_errors_on_missing_host(#[case] url_str: &str, #[case] description: &str) {
        if url_str.is_empty() {
            // Empty string cannot be parsed as URL
            return;
        }

        let url = Url::parse(url_str).unwrap();
        let result = AssetId::from_url(&url);

        assert!(result.is_err(), "{}", description);
        assert!(
            matches!(result, Err(AssetsError::MissingComponent(_))),
            "{} should return MissingComponent error",
            description
        );
    }

    #[rstest]
    #[case("https://example.com/audio.mp3", "Simple URL")]
    #[case(
        "https://example.com:8080/audio.mp3?quality=high",
        "URL with non-default port and query"
    )]
    #[case("HTTPS://EXAMPLE.COM/PATH/TO/FILE.MP3", "URL with uppercase")]
    fn test_asset_id_creation_success(#[case] url_str: &str, #[case] description: &str) {
        let url = Url::parse(url_str).unwrap();
        let result = AssetId::from_url(&url);

        assert!(
            result.is_ok(),
            "{} should create AssetId successfully",
            description
        );
    }

    #[rstest]
    #[case(
        "https://example.com/audio.mp3?token=123&quality=high#section",
        "https://example.com/audio.mp3",
        "Canonicalization for asset should remove query and fragment"
    )]
    #[case(
        "HTTPS://EXAMPLE.COM/audio.mp3",
        "https://example.com/audio.mp3",
        "Canonicalization should normalize scheme and host to lowercase"
    )]
    #[case(
        "https://example.com:443/audio.mp3",
        "https://example.com/audio.mp3",
        "Canonicalization should remove default HTTPS port 443"
    )]
    #[case(
        "http://example.com:80/audio.mp3",
        "http://example.com/audio.mp3",
        "Canonicalization should remove default HTTP port 80"
    )]
    #[case(
        "https://example.com:8443/audio.mp3",
        "https://example.com:8443/audio.mp3",
        "Canonicalization should preserve explicit non-default port"
    )]
    fn test_canonicalize_for_asset(
        #[case] input_url: &str,
        #[case] expected_canonical: &str,
        #[case] description: &str,
    ) {
        let url = Url::parse(input_url).unwrap();
        let result = canonicalize_for_asset(&url).unwrap();

        assert_eq!(result, expected_canonical, "{}", description);
    }

    #[rstest]
    #[case("file:///path/to/audio.mp3", "file URL without host should error")]
    #[case("", "Empty URL string should fail to parse")]
    fn test_canonicalize_for_asset_errors_on_missing_host(
        #[case] url_str: &str,
        #[case] description: &str,
    ) {
        if url_str.is_empty() {
            // Empty string cannot be parsed as URL
            return;
        }

        let url = Url::parse(url_str).unwrap();
        let result = canonicalize_for_asset(&url);

        assert!(result.is_err(), "{}", description);
        assert!(
            matches!(result, Err(AssetsError::MissingComponent(host)) if host == "host"),
            "{} should return MissingComponent error for host",
            description
        );
    }
}
