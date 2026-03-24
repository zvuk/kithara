use std::sync::Arc;

use bytes::Bytes;
use kithara::hls::KeyOptions;
use kithara_drm::UniqueBinaryCipher;
use rand::{distr::Alphanumeric, prelude::*};

const SEED_LEN: usize = 16;

/// Resolves DRM key at compile time. Returns the env value or default.
macro_rules! env_or_default {
    () => {
        match option_env!("KITHARA_DRM_KEY") {
            Some(k) => k,
            None => "kithara",
        }
    };
}

/// Obfuscated DRM cipher key.
///
/// Custom key via `KITHARA_DRM_KEY` env at compile time, otherwise falls back
/// to obfuscated default `"kithara"`. `obfstr!` stores XOR'd bytes in the
/// binary and decodes them at runtime.
fn drm_key() -> String {
    obfstr::obfstr!(env_or_default!()).to_string()
}

/// Build `KeyOptions` for zvuk DRM streams.
///
/// Generates a random seed, creates a cipher from `cipher_key + seed`,
/// and returns `KeyOptions` with the `X-Encrypted-Key` header and
/// a key processor that decrypts fetched keys.
pub(crate) fn make_key_options() -> KeyOptions {
    let cipher_key = drm_key();
    let seed: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(SEED_LEN)
        .map(char::from)
        .collect();

    let secret = format!("{cipher_key}{seed}");
    let cipher = UniqueBinaryCipher::new(&secret);

    let headers = [("X-Encrypted-Key".to_string(), seed)].into();

    KeyOptions::new()
        .with_request_headers(headers)
        .with_key_processor(Arc::new(move |key: Bytes, _ctx| Ok(cipher.decrypt(&key))))
}

/// Returns `true` if the URL's domain matches any of the configured DRM domains.
pub(crate) fn needs_key_cipher(url: &str, drm_domains: &[String]) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| drm_domains.iter().any(|d| h.ends_with(d.as_str())))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_key_returns_expected_default() {
        assert_eq!(drm_key(), "kithara");
    }

    #[test]
    fn needs_key_cipher_matches_domain() {
        let domains = vec!["zvq.me".to_string()];
        assert!(needs_key_cipher(
            "https://slicer-stage-k8s.zvq.me/drm/track/123/master.m3u8",
            &domains
        ));
        assert!(needs_key_cipher(
            "https://cdn-edge.zvq.me/track/streamhq?id=123",
            &domains
        ));
        assert!(!needs_key_cipher(
            "https://stream.silvercomet.top/drm/master.m3u8",
            &domains
        ));
        assert!(!needs_key_cipher(
            "https://example.com/hls/master.m3u8",
            &domains
        ));
    }
}
