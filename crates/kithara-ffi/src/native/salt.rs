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

/// Generate the zvuk.com production DRM salt required by the WAF:
/// exactly 8 lowercase-hex characters.
#[must_use]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn drm_prod_salt() -> String {
    sample(prod_spec())
}

/// Generate the zvq.me staging DRM salt required by the WAF:
/// exactly 16 ASCII alphanumeric characters.
#[must_use]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn drm_stage_salt() -> String {
    sample(stage_spec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn drm_prod_salt_returns_lowercase_hex_8() {
        let salt = drm_prod_salt();
        let spec = prod_spec();

        assert_eq!(salt.len(), spec.len);
        assert!(
            salt.bytes().all(|b| spec.alphabet.contains(&b)),
            "prod salt must be lowercase hex, got {salt:?}"
        );
    }

    #[kithara::test]
    fn drm_stage_salt_returns_alphanumeric_16() {
        let salt = drm_stage_salt();
        let spec = stage_spec();

        assert_eq!(salt.len(), spec.len);
        assert!(
            salt.chars().all(|c| c.is_ascii_alphanumeric()),
            "stage salt must be ASCII alphanumeric, got {salt:?}"
        );
        assert!(
            salt.bytes().all(|b| spec.alphabet.contains(&b)),
            "stage salt must use the legal alphabet, got {salt:?}"
        );
    }
}
