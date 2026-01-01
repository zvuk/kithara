use sha2::{Digest, Sha256};
use url::Url;

use crate::canonicalize_for_asset;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssetId([u8; 32]);

impl AssetId {
    pub fn from_url(url: &Url) -> AssetId {
        let canonical = canonicalize_for_asset(url).expect("URL canonicalization should not fail");
        let hash = Sha256::digest(canonical.as_bytes());
        AssetId(hash.into())
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
}
