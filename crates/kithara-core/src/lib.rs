#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("URL canonicalization failed: {0}")]
    Canonicalization(String),
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

pub type CoreResult<T> = Result<T, CoreError>;

fn canonicalize_for_asset(url: &url::Url) -> CoreResult<String> {
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
        .map_err(|e| CoreError::Canonicalization(e.to_string()))
}

fn canonicalize_for_resource(url: &url::Url) -> CoreResult<String> {
    let mut canonical = url.clone();

    // Remove fragment but keep query
    canonical.set_fragment(None);

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
        .map_err(|e| CoreError::Canonicalization(e.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssetId([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceHash([u8; 32]);

impl AssetId {
    pub fn from_url(url: &url::Url) -> AssetId {
        let canonical = canonicalize_for_asset(url).expect("URL canonicalization should not fail");
        let hash = Sha256::digest(canonical.as_bytes());
        AssetId(hash.into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl ResourceHash {
    pub fn from_url(url: &url::Url) -> ResourceHash {
        let canonical =
            canonicalize_for_resource(url).expect("URL canonicalization should not fail");
        let hash = Sha256::digest(canonical.as_bytes());
        ResourceHash(hash.into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_id_ignores_query_and_fragment() {
        let url1 = url::Url::parse("https://example.com/audio.mp3?token=123&quality=high#section")
            .unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?different=456#other").unwrap();
        let url3 = url::Url::parse("https://example.com/audio.mp3").unwrap();

        let asset1 = AssetId::from_url(&url1);
        let asset2 = AssetId::from_url(&url2);
        let asset3 = AssetId::from_url(&url3);

        assert_eq!(asset1, asset2);
        assert_eq!(asset1, asset3);
    }

    #[test]
    fn asset_id_normalizes_host_and_scheme_case() {
        let url1 = url::Url::parse("HTTPS://EXAMPLE.COM/audio.mp3").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3").unwrap();

        let asset1 = AssetId::from_url(&url1);
        let asset2 = AssetId::from_url(&url2);

        assert_eq!(asset1, asset2);
    }

    #[test]
    fn asset_id_removes_default_port() {
        let url1 = url::Url::parse("https://example.com:443/audio.mp3").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3").unwrap();

        let asset1 = AssetId::from_url(&url1);
        let asset2 = AssetId::from_url(&url2);

        assert_eq!(asset1, asset2);

        let url3 = url::Url::parse("http://example.com:80/audio.mp3").unwrap();
        let url4 = url::Url::parse("http://example.com/audio.mp3").unwrap();

        let asset3 = AssetId::from_url(&url3);
        let asset4 = AssetId::from_url(&url4);

        assert_eq!(asset3, asset4);
    }

    #[test]
    fn asset_id_preserves_explicit_non_default_ports() {
        let url1 = url::Url::parse("https://example.com:8443/audio.mp3").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3").unwrap();

        let asset1 = AssetId::from_url(&url1);
        let asset2 = AssetId::from_url(&url2);

        assert_ne!(asset1, asset2);
    }

    #[test]
    fn asset_id_stable_across_calls() {
        let url = url::Url::parse("https://example.com/path/to/audio.mp3?version=1.2").unwrap();

        let asset1 = AssetId::from_url(&url);
        let asset2 = AssetId::from_url(&url);

        assert_eq!(asset1, asset2);
    }

    #[test]
    fn resource_hash_includes_query_but_ignores_fragment() {
        let url1 = url::Url::parse("https://example.com/audio.mp3?token=123&quality=high#section")
            .unwrap();
        let url2 =
            url::Url::parse("https://example.com/audio.mp3?token=123&quality=high#other").unwrap();
        let url3 = url::Url::parse("https://example.com/audio.mp3?different=456").unwrap();

        let hash1 = ResourceHash::from_url(&url1);
        let hash2 = ResourceHash::from_url(&url2);
        let hash3 = ResourceHash::from_url(&url3);

        assert_eq!(hash1, hash2); // Same query, different fragment
        assert_ne!(hash1, hash3); // Different query
    }

    #[test]
    fn resource_hash_normalizes_host_and_scheme_case() {
        let url1 = url::Url::parse("HTTPS://EXAMPLE.COM/audio.mp3?token=123").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?token=123").unwrap();

        let hash1 = ResourceHash::from_url(&url1);
        let hash2 = ResourceHash::from_url(&url2);

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn resource_hash_removes_default_port() {
        let url1 = url::Url::parse("https://example.com:443/audio.mp3?token=123").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?token=123").unwrap();

        let hash1 = ResourceHash::from_url(&url1);
        let hash2 = ResourceHash::from_url(&url2);

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn resource_hash_preserves_explicit_non_default_ports() {
        let url1 = url::Url::parse("https://example.com:8443/audio.mp3?token=123").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?token=123").unwrap();

        let hash1 = ResourceHash::from_url(&url1);
        let hash2 = ResourceHash::from_url(&url2);

        assert_ne!(hash1, hash2);
    }
}
