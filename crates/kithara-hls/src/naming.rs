#![forbid(unsafe_code)]

use kithara_assets::{AssetScopeDelegate, safe_path_component, url_fingerprint};
use url::Url;

struct Consts;
impl Consts {
    const MAX_EXTENSION_LEN: usize = 16;
    const MAX_LEAF_COMPONENT_LEN: usize = 96;
}

#[derive(Debug, Default)]
pub struct HlsAssetScopeDelegate;

impl AssetScopeDelegate for HlsAssetScopeDelegate {
    fn rel_path_for_url(&self, url: &Url) -> String {
        rel_path_for_url(url)
    }
}

fn rel_path_for_url(url: &Url) -> String {
    let mut parts = vec!["hls".to_string()];
    let host = url.host_str().unwrap_or("host");
    parts.push(safe_path_component(host, url.as_str()));

    let path_parts: Vec<&str> = url
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_default();

    if path_parts.is_empty() {
        parts.push(leaf_component("index", url));
        return parts.join("/");
    }

    let prefix_len = path_parts.len().saturating_sub(1);
    parts.extend(
        path_parts
            .iter()
            .take(prefix_len)
            .map(|segment| safe_path_component(segment, url.as_str())),
    );
    if let Some(leaf) = path_parts.last() {
        parts.push(leaf_component(leaf, url));
    }
    parts.join("/")
}

fn leaf_component(leaf: &str, url: &Url) -> String {
    let fingerprint = url_fingerprint(url);
    let identity = url.as_str();
    if let Some((stem, ext)) = leaf.rsplit_once('.')
        && !stem.is_empty()
        && !ext.is_empty()
        && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
    {
        let stem = safe_path_component(stem, identity);
        let ext = extension_component(ext, identity);
        let suffix = format!("_{fingerprint}.{ext}");
        return bounded_leaf(&stem, &suffix);
    }
    let name = safe_path_component(leaf, identity);
    let suffix = format!("_{fingerprint}");
    bounded_leaf(&name, &suffix)
}

fn extension_component(ext: &str, identity: &str) -> String {
    let lower = ext.to_ascii_lowercase();
    let safe = safe_path_component(&lower, identity);
    safe.chars().take(Consts::MAX_EXTENSION_LEN).collect()
}

fn bounded_leaf(stem: &str, suffix: &str) -> String {
    let total_len = stem.len().saturating_add(suffix.len());
    if total_len <= Consts::MAX_LEAF_COMPONENT_LEN {
        return format!("{stem}{suffix}");
    }

    let keep = Consts::MAX_LEAF_COMPONENT_LEN.saturating_sub(suffix.len());
    let bounded_stem: String = stem.chars().take(keep).collect();
    format!("{bounded_stem}{suffix}")
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn hls_url_rel_path_preserves_tree_without_literal_query() {
        let url = Url::parse(
            "https://cdn.example.com/audio/v0/seg001.m4s?X-Amz-Signature=abcdefghijklmnopqrstuvwxyz",
        )
        .unwrap();

        let rel_path = rel_path_for_url(&url);

        assert!(rel_path.starts_with("hls/cdn.example.com/audio/v0/seg001_"));
        assert!(rel_path.ends_with(".m4s"));
        assert!(!rel_path.contains("X-Amz-Signature"));
        assert!(!rel_path.contains('?'));
    }

    #[kithara::test]
    fn hls_url_rel_path_bounds_long_leaf_component() {
        let long_leaf = "x".repeat(300);
        let url = Url::parse(&format!(
            "https://cdn.example.com/{long_leaf}.ts?token=secret"
        ))
        .unwrap();

        let rel_path = rel_path_for_url(&url);
        let leaf = rel_path.rsplit('/').next().unwrap();

        assert!(leaf.len() <= 96);
        assert!(leaf.ends_with(".ts"));
        assert!(!leaf.contains("token=secret"));
    }
}
