#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use url::Url;

struct Consts;
impl Consts {
    const DIGEST_PREFIX_BYTES: usize = 8;
    const MAX_PATH_COMPONENT_LEN: usize = 96;
}

/// Short stable fingerprint for names that must remain bounded while
/// preserving URL identity.
#[must_use]
pub(crate) fn url_fingerprint(url: &Url) -> String {
    short_fingerprint(url.as_str())
}

/// Convert an arbitrary label into one filesystem path component.
///
/// The output is non-empty, ASCII-only, contains no path separators, and
/// is bounded to a conservative component length. If truncation is
/// required, a fingerprint of `identity` is appended so long names do
/// not collapse to the same component.
#[must_use]
pub(crate) fn safe_path_component(label: &str, identity: &str) -> String {
    let out: String = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if out.is_empty() || out == "." || out == ".." {
        return format!("resource_{}", short_fingerprint(identity));
    }

    if out.len() <= Consts::MAX_PATH_COMPONENT_LEN {
        return out;
    }

    let fingerprint = short_fingerprint(identity);
    let keep = Consts::MAX_PATH_COMPONENT_LEN.saturating_sub(fingerprint.len() + 1);
    let mut bounded: String = out.chars().take(keep).collect();
    bounded.push('_');
    bounded.push_str(&fingerprint);
    bounded
}

fn short_fingerprint(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(&digest[..Consts::DIGEST_PREFIX_BYTES])
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn safe_path_component_bounds_long_values() {
        let label = "x".repeat(300);
        let safe = safe_path_component(&label, "identity");

        assert!(safe.len() <= Consts::MAX_PATH_COMPONENT_LEN);
        assert!(safe.is_ascii());
    }

    #[kithara::test]
    #[case("")]
    #[case(".")]
    #[case("..")]
    fn safe_path_component_replaces_empty_or_dot_segments(#[case] label: &str) {
        let safe = safe_path_component(label, "identity");

        assert!(safe.starts_with("resource_"));
        assert_ne!(safe, ".");
        assert_ne!(safe, "..");
    }
}
