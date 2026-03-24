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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_key_returns_expected_default() {
        assert_eq!(drm_key(), "kithara");
    }
}
