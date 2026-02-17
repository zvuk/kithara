#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::{AssetsError, AssetsResult};

/// Key type for addressing resources within an asset.
///
/// A `ResourceKey` identifies a single resource (file) within an asset store.
///
/// Two variants:
/// - `Relative`: path within `<root_dir>/<asset_root>` (existing behavior).
/// - `Absolute`: filesystem path used directly, bypassing `root/asset_root` resolution.
///   Used for local files opened via `AssetStore`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceKey {
    /// Relative path within `asset_root` (existing behavior).
    Relative(String),
    /// Absolute filesystem path — bypasses `root_dir/asset_root` resolution.
    /// Used for local files opened directly.
    Absolute(PathBuf),
}

impl ResourceKey {
    /// Create a relative resource key (backward-compatible).
    pub fn new<S: Into<String>>(rel_path: S) -> Self {
        Self::Relative(rel_path.into())
    }

    /// Create an absolute resource key for a local file.
    pub fn absolute<P: Into<PathBuf>>(path: P) -> Self {
        Self::Absolute(path.into())
    }

    /// Extracts the relative path (last path segment) from a URL.
    #[must_use]
    pub fn from_url(url: &Url) -> Self {
        let rel_path = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("index")
            .to_string();
        Self::Relative(rel_path)
    }

    /// Returns true if this is an absolute path key.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        matches!(self, Self::Absolute(_))
    }

    /// Returns the absolute path if this is an Absolute key.
    #[must_use]
    pub fn as_absolute_path(&self) -> Option<&Path> {
        match self {
            Self::Absolute(p) => Some(p),
            Self::Relative(_) => None,
        }
    }
}

/// Generates an asset root identifier from a URL and an optional name.
///
/// This is typically used with the master playlist URL to create a unique
/// directory name for all resources of an HLS stream.
///
/// Uses canonicalization to ensure consistency:
/// URLs that differ only in case, default ports, or query/fragment
/// will produce the same asset root.
///
/// When `name` is provided, it is included in the hash so that resources
/// with the same canonical URL but different names produce distinct roots.
/// This is useful when URLs differ only in query parameters (which are
/// stripped during canonicalization).
#[must_use]
pub fn asset_root_for_url(url: &Url, name: Option<&str>) -> String {
    // Use canonical form for consistent hashing.
    // Fall back to raw URL if canonicalization fails (e.g., file:// URLs).
    let canonical = canonicalize_for_asset(url).unwrap_or_else(|_| url.as_str().to_string());

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    if let Some(name) = name {
        hasher.update(b":");
        hasher.update(name.as_bytes());
    }
    let hash_result = hasher.finalize();
    hex::encode(&hash_result[..16])
}

/// Canonicalizes a URL for asset identification.
///
/// This function normalizes URLs to ensure consistent identification:
/// - Removes query parameters and fragments
/// - Normalizes scheme and host to lowercase
/// - Removes default ports (80 for HTTP, 443 for HTTPS)
///
/// # Errors
///
/// Returns `AssetsError::MissingComponent` if the URL lacks a scheme or host.
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
    use std::path::PathBuf;

    use rstest::rstest;
    use url::Url;

    use super::*;
    use crate::AssetsError;

    #[rstest]
    #[case(
        "https://example.com/audio.mp3?token=123&quality=high#section",
        "https://example.com/audio.mp3?different=456#other",
        true,
        "asset_root should ignore both query and fragment"
    )]
    #[case(
        "https://example.com/audio.mp3?token=123&quality=high#section",
        "https://example.com/audio.mp3",
        true,
        "asset_root should ignore query and fragment compared to clean URL"
    )]
    #[case(
        "HTTPS://EXAMPLE.COM/audio.mp3",
        "https://example.com/audio.mp3",
        true,
        "asset_root should normalize scheme and host to lowercase"
    )]
    #[case(
        "https://example.com:443/audio.mp3",
        "https://example.com/audio.mp3",
        true,
        "asset_root should remove default HTTPS port 443"
    )]
    #[case(
        "http://example.com:80/audio.mp3",
        "http://example.com/audio.mp3",
        true,
        "asset_root should remove default HTTP port 80"
    )]
    #[case(
        "https://example.com:8443/audio.mp3",
        "https://example.com/audio.mp3",
        false,
        "asset_root should preserve explicit non-default port"
    )]
    fn test_asset_root_comparisons(
        #[case] url1: &str,
        #[case] url2: &str,
        #[case] should_be_equal: bool,
        #[case] description: &str,
    ) {
        let url1 = Url::parse(url1).unwrap();
        let url2 = Url::parse(url2).unwrap();

        let root1 = asset_root_for_url(&url1, None);
        let root2 = asset_root_for_url(&url2, None);

        if should_be_equal {
            assert_eq!(root1, root2, "{}", description);
        } else {
            assert_ne!(root1, root2, "{}", description);
        }
    }

    #[test]
    fn test_asset_root_stable_across_calls() {
        let url = Url::parse("https://example.com/path/to/audio.mp3?version=1.2").unwrap();

        let root1 = asset_root_for_url(&url, None);
        let root2 = asset_root_for_url(&url, None);

        assert_eq!(
            root1, root2,
            "asset_root should be stable across multiple calls"
        );
    }

    #[rstest]
    #[case("https://example.com/audio.mp3", "Simple URL")]
    #[case(
        "https://example.com:8080/audio.mp3?quality=high",
        "URL with non-default port and query"
    )]
    #[case("HTTPS://EXAMPLE.COM/PATH/TO/FILE.MP3", "URL with uppercase")]
    fn test_asset_root_creation_success(#[case] url_str: &str, #[case] _description: &str) {
        let url = Url::parse(url_str).unwrap();
        let root = asset_root_for_url(&url, None);

        // Should return a non-empty hex string
        assert!(!root.is_empty());
        assert!(root.chars().all(|c| c.is_ascii_hexdigit()));
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

    #[test]
    fn test_asset_root_with_name_differs_from_without() {
        let url = Url::parse("https://example.com/stream").unwrap();

        let root_no_name = asset_root_for_url(&url, None);
        let root_with_name = asset_root_for_url(&url, Some("track_123"));

        assert_ne!(
            root_no_name, root_with_name,
            "asset_root with name should differ from without"
        );
    }

    #[test]
    fn test_asset_root_same_url_different_names_differ() {
        let url = Url::parse("https://cdn.example.com/stream").unwrap();

        let root_a = asset_root_for_url(&url, Some("track_123"));
        let root_b = asset_root_for_url(&url, Some("track_456"));

        assert_ne!(
            root_a, root_b,
            "same URL with different names should produce different asset roots"
        );
    }

    #[test]
    fn test_asset_root_same_url_same_name_equal() {
        let url = Url::parse("https://cdn.example.com/stream").unwrap();

        let root_a = asset_root_for_url(&url, Some("track_123"));
        let root_b = asset_root_for_url(&url, Some("track_123"));

        assert_eq!(
            root_a, root_b,
            "same URL with same name should produce identical asset roots"
        );
    }

    #[test]
    fn test_asset_root_query_differs_but_name_distinguishes() {
        // URLs that differ only in query params — canonicalization strips them.
        let url1 = Url::parse("https://cdn.example.com/stream?track_id=123&token=abc").unwrap();
        let url2 = Url::parse("https://cdn.example.com/stream?track_id=456&token=def").unwrap();

        // Without name: same root (the bug scenario).
        let root1_no_name = asset_root_for_url(&url1, None);
        let root2_no_name = asset_root_for_url(&url2, None);
        assert_eq!(
            root1_no_name, root2_no_name,
            "without name, query-only differences produce same root"
        );

        // With distinct names: different roots (the fix).
        let root1_named = asset_root_for_url(&url1, Some("track_123"));
        let root2_named = asset_root_for_url(&url2, Some("track_456"));
        assert_ne!(
            root1_named, root2_named,
            "with different names, same canonical URL produces different roots"
        );
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

    #[test]
    fn test_resource_key_absolute() {
        let path = PathBuf::from("/tmp/song.mp3");
        let key = ResourceKey::absolute(&path);
        assert!(key.is_absolute());
        assert_eq!(key.as_absolute_path(), Some(path.as_path()));
    }

    #[test]
    fn test_resource_key_relative_is_not_absolute() {
        let key = ResourceKey::new("seg.m4s");
        assert!(!key.is_absolute());
        assert_eq!(key.as_absolute_path(), None);
    }

    #[test]
    fn test_resource_key_absolute_hash_differs() {
        use std::collections::HashSet;
        let abs = ResourceKey::absolute("/tmp/a.mp3");
        let rel = ResourceKey::new("/tmp/a.mp3");
        let mut set = HashSet::new();
        set.insert(abs.clone());
        assert_ne!(abs, rel);
    }
}
