use sha2::{Digest, Sha256};
use url::Url;

use crate::canonicalize_for_resource;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceHash([u8; 32]);

impl ResourceHash {
    pub fn from_url(url: &Url) -> ResourceHash {
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
