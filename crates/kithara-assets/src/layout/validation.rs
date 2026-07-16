#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};

use super::AssetSource;
use crate::{AssetsError, AssetsResult};

pub(crate) const MAX_COMPONENT_LEN: usize = 96;

struct Consts;
impl Consts {
    const HASH_PREFIX_BYTES: usize = 16;
    const MAX_EXTENSION_LEN: usize = 16;
}

pub(crate) fn append_component_suffix(component: &str, suffix: &str, identity: &[u8]) -> String {
    let mut encoded = String::with_capacity(component.len() + suffix.len());
    encoded.push_str(component);
    encoded.push_str(suffix);
    finish_component(encoded, identity)
}

#[must_use]
pub(crate) fn encode_component(value: &str) -> String {
    let encoded = encode_bytes(value, false);
    finish_component(encoded, value.as_bytes())
}

#[must_use]
pub(crate) fn encode_url_component(value: &str) -> String {
    if value.is_empty() {
        return "~e".to_string();
    }
    let encoded = encode_bytes(value, true);
    finish_component(encoded, value.as_bytes())
}

#[must_use]
pub(crate) fn encode_url_leaf(leaf: &str, query: Option<&str>) -> String {
    let query = query.filter(|value| !value.is_empty());
    let query_suffix = query.map_or_else(String::new, |value| {
        format!("~q{}", fingerprint(value.as_bytes()))
    });

    let mut encoded = if leaf.is_empty() {
        "~e".to_string()
    } else if let Some((stem, extension)) = usable_extension(leaf) {
        let mut value = encode_bytes(stem, true);
        value.push_str(&query_suffix);
        value.push('.');
        value.push_str(&encode_bytes(extension, true));
        value
    } else {
        let mut value = encode_bytes(leaf, true);
        value.push_str(&query_suffix);
        value
    };

    if leaf.is_empty() {
        encoded.push_str(&query_suffix);
    }

    let mut identity =
        Vec::with_capacity(leaf.len() + query.map_or(0, str::len) + usize::from(query.is_some()));
    identity.extend_from_slice(leaf.as_bytes());
    if let Some(query) = query {
        identity.push(0);
        identity.extend_from_slice(query.as_bytes());
    }
    finish_component(encoded, &identity)
}

#[must_use]
pub(crate) fn fingerprint(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    hex::encode(&digest[..Consts::HASH_PREFIX_BYTES])
}

pub(crate) fn validate_path(path: &str) -> AssetsResult<()> {
    if path.is_empty() || path.starts_with(['/', '\\']) {
        return Err(AssetsError::InvalidKey);
    }

    for component in path.split('/') {
        validate_component(component)?;
        if has_temp_suffix(component) {
            return Err(AssetsError::InvalidKey);
        }
    }
    Ok(())
}

pub(crate) fn validate_root(root: &str) -> AssetsResult<()> {
    validate_component(root)?;
    if root.eq_ignore_ascii_case("_index") {
        return Err(AssetsError::InvalidKey);
    }
    Ok(())
}

pub(crate) fn validate_source(source: &AssetSource) -> AssetsResult<()> {
    let valid = match source {
        AssetSource::Remote { url, .. } => {
            !url.scheme().is_empty() && url.host().is_some() && url.path_segments().is_some()
        }
        AssetSource::Local { path } => path.is_absolute(),
    };
    if valid {
        Ok(())
    } else {
        Err(AssetsError::InvalidKey)
    }
}

fn encode_bytes(value: &str, preserve_percent_triplets: bool) -> String {
    let bytes = value.as_bytes();
    let mut encoded = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if preserve_percent_triplets
            && byte == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            encoded.push_str("~p");
            encoded.push(char::from(bytes[index + 1].to_ascii_lowercase()));
            encoded.push(char::from(bytes[index + 2].to_ascii_lowercase()));
            index += 3;
            continue;
        }

        if byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        {
            encoded.push(char::from(byte));
        } else {
            push_byte_escape(&mut encoded, byte);
        }
        index += 1;
    }
    encoded
}

fn finish_component(mut encoded: String, identity: &[u8]) -> String {
    if encoded.is_empty() {
        return encoded;
    }

    let trailing_dots = encoded
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'.')
        .count();
    if trailing_dots > 0 {
        encoded.truncate(encoded.len() - trailing_dots);
        for _ in 0..trailing_dots {
            encoded.push_str("~2e");
        }
    }

    if has_temp_suffix(&encoded) {
        let dot = encoded.len() - 4;
        encoded.replace_range(dot..=dot, "~2e");
    }
    if is_windows_device_name(&encoded) {
        encoded.insert_str(0, "~r");
    }

    if encoded.len() <= MAX_COMPONENT_LEN {
        return encoded;
    }

    let suffix = format!("~h{}", fingerprint(identity));
    encoded.truncate(MAX_COMPONENT_LEN - suffix.len());
    encoded.push_str(&suffix);
    encoded
}

fn has_temp_suffix(component: &str) -> bool {
    component
        .get(component.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".tmp"))
}

fn is_windows_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.']);
    if ["con", "prn", "aux", "nul", "conin$", "conout$", "clock$"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return true;
    }

    stem.len() == 4
        && (stem[..3].eq_ignore_ascii_case("com") || stem[..3].eq_ignore_ascii_case("lpt"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9')
}

fn push_byte_escape(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    output.push('~');
    output.push(char::from(HEX[usize::from(byte >> 4)]));
    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

fn usable_extension(leaf: &str) -> Option<(&str, &str)> {
    let (stem, extension) = leaf.rsplit_once('.')?;
    (!stem.is_empty()
        && !extension.is_empty()
        && extension.len() <= Consts::MAX_EXTENSION_LEN
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    .then_some((stem, extension))
}

fn validate_component(component: &str) -> AssetsResult<()> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.len() > MAX_COMPONENT_LEN
        || !component.is_ascii()
        || component.ends_with(['.', ' '])
        || component.bytes().any(|byte| {
            byte <= 0x1f
                || byte == 0x7f
                || matches!(
                    byte,
                    b'<' | b'>' | b':' | b'"' | b'/' | b'\\' | b'|' | b'?' | b'*'
                )
        })
        || is_windows_device_name(component)
    {
        return Err(AssetsError::InvalidKey);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;

    #[kithara::test]
    fn component_encoding_is_injective_and_case_safe() {
        assert_eq!(encode_component("analysis"), "analysis");
        assert_eq!(encode_component("A/a~"), "~41~2fa~7e");
        assert_ne!(encode_component("A"), encode_component("a"));
        assert_eq!(encode_component("con"), "~rcon");
        assert_eq!(encode_component("foo."), "foo~2e");
        assert_eq!(encode_component("foo.tmp"), "foo~2etmp");
    }

    #[kithara::test]
    fn component_encoding_bounds_distinct_long_values() {
        let first = format!("{}a", "x".repeat(300));
        let second = format!("{}b", "x".repeat(300));
        let first = encode_component(&first);
        let second = encode_component(&second);

        assert_eq!(first.len(), MAX_COMPONENT_LEN);
        assert_eq!(second.len(), MAX_COMPONENT_LEN);
        assert_ne!(first, second);
    }

    #[kithara::test]
    #[case("", false)]
    #[case("root", true)]
    #[case("ROOT", true)]
    #[case("a/b", false)]
    #[case("..", false)]
    #[case("_index", false)]
    #[case("_INDEX", false)]
    #[case("con", false)]
    #[case("trailing.", false)]
    #[case("caf\u{e9}", false)]
    fn root_validation_is_portable(#[case] root: &str, #[case] valid: bool) {
        assert_eq!(validate_root(root).is_ok(), valid);
    }

    #[kithara::test]
    #[case("track/track.mp3", true)]
    #[case("analysis/track.analysis", true)]
    #[case("", false)]
    #[case("/absolute", false)]
    #[case("dir//file", false)]
    #[case("dir/./file", false)]
    #[case("dir/../file", false)]
    #[case("dir\\file", false)]
    #[case("dir/con.txt", false)]
    #[case("dir.tmp/file", false)]
    #[case("dir/file.tmp", false)]
    #[case("dir/file.TMP", false)]
    fn path_validation_is_portable(#[case] path: &str, #[case] valid: bool) {
        assert_eq!(validate_path(path).is_ok(), valid);
    }

    #[kithara::test]
    fn source_validation_rejects_unaddressable_sources() {
        let remote = AssetSource::Remote {
            url: Url::parse("https://example.com/track.mp3").expect("remote URL"),
            discriminator: None,
        };
        let file_url = AssetSource::Remote {
            url: Url::parse("file:///tmp/track.mp3").expect("file URL"),
            discriminator: None,
        };
        let local = AssetSource::Local {
            path: PathBuf::from("relative.mp3"),
        };

        assert!(validate_source(&remote).is_ok());
        assert!(validate_source(&file_url).is_err());
        assert!(validate_source(&local).is_err());
    }

    #[kithara::test]
    fn url_encoding_preserves_percent_and_empty_segment_identity() {
        assert_eq!(encode_url_component("a%2Fb"), "a~p2fb");
        assert_eq!(encode_url_component("a%2fb"), "a~p2fb");
        assert_eq!(encode_url_component(""), "~e");
        assert_ne!(encode_url_component("a%2Fb"), encode_url_component("a/b"));
    }

    #[kithara::test]
    fn url_leaf_hashes_only_nonempty_query() {
        assert_eq!(encode_url_leaf("master.m3u8", None), "master.m3u8");
        assert_eq!(
            encode_url_leaf("master.m3u8", Some("token=secret")),
            concat!("master~q1359227d95e1b", "a804467fce441aeb48f.m3u8")
        );
        assert_eq!(encode_url_leaf("master.m3u8", Some("")), "master.m3u8");
    }
}
