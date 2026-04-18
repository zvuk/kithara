use std::{collections::HashMap, sync::Arc};

use kithara_drm::{KeyProcessorRegistry, KeyProcessorRule, UniqueBinaryCipher};
use rand::{distr::Alphanumeric, prelude::*};

const SEED_LEN: usize = 16;

/// Resolves DRM key at compile time. Returns the env value or default.
macro_rules! env_or_default {
    () => {
        match option_env!("KITHARA_DRM_KEY") {
            Some(k) => k,
            None => "BinaryCipherKey",
        }
    };
}

/// Obfuscated DRM cipher key.
fn drm_key() -> String {
    obfstr::obfstr!(env_or_default!()).to_string()
}

/// Auth token baked in at build time from `KITHARA_DRM_AUTH_TOKEN`.
/// `None` when the env var was unset during compilation — in which case
/// `X-Auth-Token` is simply omitted from key requests and the key
/// server returns 401 for authenticated tracks. Production builds
/// should supply the token; dev builds work fine without it for
/// non-DRM content.
fn compile_time_zvq_auth_token() -> Option<&'static str> {
    option_env!("KITHARA_DRM_AUTH_TOKEN")
}

/// Build the default registry for `*.zvq.me` DRM tracks, using the
/// auth token baked in at compile time (when available).
#[must_use]
pub fn default_zvq_key_registry() -> KeyProcessorRegistry {
    make_key_registry(
        &["*.zvq.me".to_string()],
        compile_time_zvq_auth_token().map(ToString::to_string),
    )
}

/// Build a [`KeyProcessorRegistry`] scoped to `domains`.
///
/// Generates a random seed, creates a cipher from `cipher_key + seed`,
/// and binds `X-Encrypted-Key` (always) and `X-Auth-Token` (when
/// `auth_token` is `Some`) headers to the given domain patterns. Key
/// URLs that don't match any pattern pass through unmodified
/// (silvercomet keys, which need no auth).
pub fn make_key_registry(domains: &[String], auth_token: Option<String>) -> KeyProcessorRegistry {
    let cipher_key = drm_key();
    let seed: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(SEED_LEN)
        .map(char::from)
        .collect();

    let secret = format!("{cipher_key}{seed}");
    let cipher = UniqueBinaryCipher::new(&secret);
    let mut headers = HashMap::new();
    headers.insert("X-Encrypted-Key".to_string(), seed);
    if let Some(token) = auth_token {
        headers.insert("X-Auth-Token".to_string(), token);
    }

    let rule = KeyProcessorRule::new(domains, Arc::new(move |key| Ok(cipher.decrypt(&key))))
        .with_headers(headers);

    let mut registry = KeyProcessorRegistry::new();
    registry.add(rule);
    registry
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn drm_key_returns_expected_default() {
        assert_eq!(drm_key(), "BinaryCipherKey");
    }
}
