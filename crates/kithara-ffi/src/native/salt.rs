use rand::prelude::*;

#[derive(Clone, Copy)]
struct SaltSpec {
    alphabet: &'static [u8],
    len: usize,
}

fn prod_spec() -> SaltSpec {
    SaltSpec {
        alphabet: b"0123456789abcdef",
        len: 8,
    }
}

fn stage_spec() -> SaltSpec {
    SaltSpec {
        alphabet: b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
        len: 16,
    }
}

fn sample(spec: SaltSpec) -> String {
    let alphabet = spec.alphabet;
    debug_assert!(!alphabet.is_empty());

    let mut rng = rand::rng();
    let mut out = String::with_capacity(spec.len);
    for _ in 0..spec.len {
        let idx = rng.random_range(0..alphabet.len());
        out.push(char::from(alphabet[idx]));
    }
    out
}

/// Generate an 8-character lowercase-hex DRM salt.
#[must_use]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn drm_lowercase_hex_salt() -> String {
    sample(prod_spec())
}

/// Generate a 16-character ASCII alphanumeric DRM salt.
#[must_use]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn drm_ascii_alphanumeric_salt() -> String {
    sample(stage_spec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn drm_lowercase_hex_salt_returns_8_chars() {
        let salt = drm_lowercase_hex_salt();
        let spec = prod_spec();

        assert_eq!(salt.len(), spec.len);
        assert!(
            salt.bytes().all(|b| spec.alphabet.contains(&b)),
            "lowercase-hex salt must use the legal alphabet, got {salt:?}"
        );
    }

    #[kithara::test]
    fn drm_ascii_alphanumeric_salt_returns_16_chars() {
        let salt = drm_ascii_alphanumeric_salt();
        let spec = stage_spec();

        assert_eq!(salt.len(), spec.len);
        assert!(
            salt.chars().all(|c| c.is_ascii_alphanumeric()),
            "ASCII-alphanumeric salt must be alphanumeric, got {salt:?}"
        );
        assert!(
            salt.bytes().all(|b| spec.alphabet.contains(&b)),
            "ASCII-alphanumeric salt must use the legal alphabet, got {salt:?}"
        );
    }
}
