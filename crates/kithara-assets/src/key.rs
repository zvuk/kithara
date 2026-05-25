#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::{AssetsError, AssetsResult};

/// Self-identifying key for a single resource (file) within an asset store.
///
/// Two variants:
/// - `Relative`: a `rel_path` under an `asset_root` namespace.
/// - `Absolute`: a filesystem path used directly; its own namespace,
///   bypassing `root_dir`/`asset_root` resolution. Used for local files.
///
/// Mint relative keys through [`crate::AssetScope`], which holds the
/// `asset_root`. `Arc<str>` keeps clones into the cache and HLS
/// bookkeeping maps cheap.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceKey {
    /// `rel_path` under an `asset_root` namespace.
    Relative {
        #[serde(with = "arc_str")]
        asset_root: Arc<str>,
        #[serde(with = "arc_str")]
        rel_path: Arc<str>,
    },
    /// Absolute filesystem path — its own namespace.
    Absolute(PathBuf),
}

/// Serialize `Arc<str>` as a plain string; the workspace serde has no
/// `rc` feature, so the reference-counting is dropped on the wire.
mod arc_str {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(value: &Arc<str>, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(value)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Arc<str>, D::Error> {
        let s = String::deserialize(de)?;
        Ok(Arc::from(s.as_str()))
    }
}

impl ResourceKey {
    /// Create an absolute resource key for a local file.
    pub fn absolute<P: Into<PathBuf>>(path: P) -> Self {
        Self::Absolute(path.into())
    }

    /// Create a relative key under `asset_root`.
    pub(crate) fn relative(asset_root: impl Into<Arc<str>>, rel_path: impl Into<Arc<str>>) -> Self {
        Self::Relative {
            asset_root: asset_root.into(),
            rel_path: rel_path.into(),
        }
    }

    /// Derive a unique `rel_path` from a URL.
    ///
    /// Last non-empty path segment (or `"index"`), plus a `_<query>`
    /// suffix when a query is present so URLs differing only in query
    /// (e.g. `?id=123` vs `?id=456`) produce distinct keys.
    pub(crate) fn rel_path_from_url(url: &Url) -> String {
        let segment = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("index");
        url.query()
            .map_or_else(|| segment.to_string(), |q| format!("{segment}_{q}"))
    }

    /// The asset namespace, or `None` for an absolute key.
    #[must_use]
    pub fn asset_root(&self) -> Option<&str> {
        match self {
            Self::Relative { asset_root, .. } => Some(asset_root),
            Self::Absolute(_) => None,
        }
    }

    /// The relative path, or `None` for an absolute key.
    #[must_use]
    pub fn rel_path(&self) -> Option<&str> {
        match self {
            Self::Relative { rel_path, .. } => Some(rel_path),
            Self::Absolute(_) => None,
        }
    }

    /// Returns the absolute path if this is an Absolute key.
    #[must_use]
    pub fn as_absolute_path(&self) -> Option<&Path> {
        match self {
            Self::Absolute(p) => Some(p),
            Self::Relative { .. } => None,
        }
    }

    /// Returns true if this is an absolute path key.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        matches!(self, Self::Absolute(_))
    }

    /// Whether this relative key names the same resource as `url` would
    /// under its asset root. An absolute key never matches a URL.
    #[must_use]
    pub fn matches_url(&self, url: &Url) -> bool {
        match self {
            Self::Relative { rel_path, .. } => **rel_path == Self::rel_path_from_url(url),
            Self::Absolute(_) => false,
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
    const HASH_PREFIX_LEN: usize = 16;

    let canonical = canonicalize_for_asset(url).unwrap_or_else(|_| url.as_str().to_string());

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    if let Some(name) = name {
        hasher.update(b":");
        hasher.update(name.as_bytes());
    }
    let hash_result = hasher.finalize();
    hex::encode(&hash_result[..HASH_PREFIX_LEN])
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
pub(crate) fn canonicalize_for_asset(url: &Url) -> AssetsResult<String> {
    /// Default HTTPS port.
    const DEFAULT_HTTPS_PORT: u16 = 443;
    /// Default HTTP port.
    const DEFAULT_HTTP_PORT: u16 = 80;

    if url.scheme().is_empty() {
        return Err(AssetsError::MissingComponent("scheme".to_string()));
    }
    if url.host().is_none() {
        return Err(AssetsError::MissingComponent("host".to_string()));
    }

    let mut canonical = url.clone();

    canonical.set_fragment(None);
    canonical.set_query(None);

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

    match (canonical.scheme(), canonical.port()) {
        ("https", Some(DEFAULT_HTTPS_PORT)) | ("http", Some(DEFAULT_HTTP_PORT)) => {
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
    use std::{collections::HashSet, path::PathBuf};

    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;
    use crate::AssetsError;

    #[kithara::test]
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

    #[kithara::test]
    fn test_asset_root_stable_across_calls() {
        let url = Url::parse("https://example.com/path/to/audio.mp3?version=1.2").unwrap();

        let root1 = asset_root_for_url(&url, None);
        let root2 = asset_root_for_url(&url, None);

        assert_eq!(
            root1, root2,
            "asset_root should be stable across multiple calls"
        );
    }

    #[kithara::test]
    #[case("https://example.com/audio.mp3", "Simple URL")]
    #[case(
        "https://example.com:8080/audio.mp3?quality=high",
        "URL with non-default port and query"
    )]
    #[case("HTTPS://EXAMPLE.COM/PATH/TO/FILE.MP3", "URL with uppercase")]
    fn test_asset_root_creation_success(#[case] url_str: &str, #[case] _description: &str) {
        let url = Url::parse(url_str).unwrap();
        let root = asset_root_for_url(&url, None);

        assert!(!root.is_empty());
        assert!(root.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[kithara::test]
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

    #[kithara::test]
    #[case::name_vs_no_name("https://example.com/stream", None, Some("track_123"), false)]
    #[case::different_names(
        "https://cdn.example.com/stream",
        Some("track_123"),
        Some("track_456"),
        false
    )]
    #[case::same_name(
        "https://cdn.example.com/stream",
        Some("track_123"),
        Some("track_123"),
        true
    )]
    fn test_asset_root_name_distinguishes(
        #[case] url_str: &str,
        #[case] name_a: Option<&str>,
        #[case] name_b: Option<&str>,
        #[case] should_equal: bool,
    ) {
        let url = Url::parse(url_str).unwrap();
        let root_a = asset_root_for_url(&url, name_a);
        let root_b = asset_root_for_url(&url, name_b);
        if should_equal {
            assert_eq!(root_a, root_b);
        } else {
            assert_ne!(root_a, root_b);
        }
    }

    #[kithara::test]
    fn test_asset_root_query_differs_but_name_distinguishes() {
        let url1 = Url::parse("https://cdn.example.com/stream?track_id=123&token=abc").unwrap();
        let url2 = Url::parse("https://cdn.example.com/stream?track_id=456&token=def").unwrap();

        let root1_no_name = asset_root_for_url(&url1, None);
        let root2_no_name = asset_root_for_url(&url2, None);
        assert_eq!(
            root1_no_name, root2_no_name,
            "without name, query-only differences produce same root"
        );

        let root1_named = asset_root_for_url(&url1, Some("track_123"));
        let root2_named = asset_root_for_url(&url2, Some("track_456"));
        assert_ne!(
            root1_named, root2_named,
            "with different names, same canonical URL produces different roots"
        );
    }

    #[kithara::test]
    #[case("file:///path/to/audio.mp3", "file URL without host should error")]
    #[case("", "Empty URL string should fail to parse")]
    fn test_canonicalize_for_asset_errors_on_missing_host(
        #[case] url_str: &str,
        #[case] description: &str,
    ) {
        if url_str.is_empty() {
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

    #[kithara::test]
    fn test_resource_key_absolute() {
        let path = PathBuf::from("/tmp/song.mp3");
        let key = ResourceKey::absolute(&path);
        assert!(key.is_absolute());
        assert_eq!(key.as_absolute_path(), Some(path.as_path()));
    }

    #[kithara::test]
    fn test_resource_key_relative_is_not_absolute() {
        let key = ResourceKey::relative("root", "seg.m4s");
        assert!(!key.is_absolute());
        assert_eq!(key.as_absolute_path(), None);
        assert_eq!(key.asset_root(), Some("root"));
        assert_eq!(key.rel_path(), Some("seg.m4s"));
    }

    #[kithara::test]
    fn test_resource_key_absolute_hash_differs() {
        let abs = ResourceKey::absolute("/tmp/a.mp3");
        let rel = ResourceKey::relative("root", "/tmp/a.mp3");
        let mut set = HashSet::new();
        set.insert(abs.clone());
        assert_ne!(abs, rel);
        assert_eq!(abs.asset_root(), None);
    }

    /// The asset namespace is part of relative key identity: two scopes
    /// over different roots mint unequal, distinctly-hashing keys for the
    /// same `rel_path`; an absolute key is its own namespace, unaffected.
    #[kithara::test]
    fn test_relative_key_identity_carries_asset_root() {
        let a = ResourceKey::relative("root_a", "seg.m4s");
        let b = ResourceKey::relative("root_b", "seg.m4s");
        assert_ne!(a, b, "same rel_path under different roots must differ");

        let mut set = HashSet::new();
        set.insert(a.clone());
        assert!(
            !set.contains(&b),
            "distinct roots must not collide in a set"
        );
        set.insert(b);
        assert_eq!(set.len(), 2);

        let same = ResourceKey::relative("root_a", "seg.m4s");
        assert_eq!(a, same, "same root + rel_path must be equal");

        let abs = ResourceKey::absolute("/tmp/seg.m4s");
        assert_ne!(abs, a);
        assert_eq!(abs.asset_root(), None);
    }
}
