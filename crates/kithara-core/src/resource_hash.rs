use sha2::{Digest, Sha256};
use url::Url;

use crate::{CoreResult, canonicalize_for_resource};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceHash([u8; 32]);

impl ResourceHash {
    pub fn from_url(url: &Url) -> CoreResult<ResourceHash> {
        let canonical = canonicalize_for_resource(url)?;
        let hash = Sha256::digest(canonical.as_bytes());
        Ok(ResourceHash(hash.into()))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_hash_includes_query_but_ignores_fragment() {
        let url1 = url::Url::parse("https://example.com/audio.mp3?token=123&quality=high#section")
            .unwrap();
        let url2 =
            url::Url::parse("https://example.com/audio.mp3?token=123&quality=high#other").unwrap();
        let url3 = url::Url::parse("https://example.com/audio.mp3?different=456").unwrap();

        let hash1 = ResourceHash::from_url(&url1).unwrap();
        let hash2 = ResourceHash::from_url(&url2).unwrap();
        let hash3 = ResourceHash::from_url(&url3).unwrap();

        assert_eq!(hash1, hash2); // Same query, different fragment
        assert_ne!(hash1, hash3); // Different query
    }

    #[test]
    fn resource_hash_normalizes_host_and_scheme_case() {
        let url1 = url::Url::parse("HTTPS://EXAMPLE.COM/audio.mp3?token=123").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?token=123").unwrap();

        let hash1 = ResourceHash::from_url(&url1).unwrap();
        let hash2 = ResourceHash::from_url(&url2).unwrap();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn resource_hash_removes_default_port() {
        let url1 = url::Url::parse("https://example.com:443/audio.mp3?token=123").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?token=123").unwrap();

        let hash1 = ResourceHash::from_url(&url1).unwrap();
        let hash2 = ResourceHash::from_url(&url2).unwrap();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn resource_hash_preserves_explicit_non_default_ports() {
        let url1 = url::Url::parse("https://example.com:8443/audio.mp3?token=123").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?token=123").unwrap();

        let hash1 = ResourceHash::from_url(&url1).unwrap();
        let hash2 = ResourceHash::from_url(&url2).unwrap();

        assert_ne!(hash1, hash2);
    }



    #[test]
    fn resource_hash_errors_on_missing_host() {
        // Create URL without host
        let url = url::Url::parse("file:///path/to/audio.mp3?token=123").unwrap();

        let result = ResourceHash::from_url(&url);
        assert!(result.is_err());
        assert!(matches!(result, Err(crate::CoreError::MissingComponent(_))));
    }
}
