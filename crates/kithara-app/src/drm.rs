use std::sync::Arc;

use kithara_drm::{KeyProcessorRegistry, KeyProcessorRule, UniqueBinaryCipher};
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
fn drm_key() -> String {
    obfstr::obfstr!(env_or_default!()).to_string()
}

/// Build a [`KeyProcessorRegistry`] scoped to `domains`.
///
/// Generates a random seed, creates a cipher from `cipher_key + seed`,
/// and binds the processor + `X-Encrypted-Key` header to the given
/// domain patterns. Key URLs that don't match any pattern pass through
/// unmodified.
pub fn make_key_registry(domains: &[String]) -> KeyProcessorRegistry {
    let cipher_key = drm_key();
    let seed: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(SEED_LEN)
        .map(char::from)
        .collect();

    let secret = format!("{cipher_key}{seed}");
    let cipher = UniqueBinaryCipher::new(&secret);
    let headers = [("X-Encrypted-Key".to_string(), seed)].into();

    let rule = KeyProcessorRule::new(domains, Arc::new(move |key| Ok(cipher.decrypt(&key))))
        .with_headers(headers);

    let mut registry = KeyProcessorRegistry::new();
    registry.add(rule);
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_key_returns_expected_default() {
        assert_eq!(drm_key(), "kithara");
    }
}
