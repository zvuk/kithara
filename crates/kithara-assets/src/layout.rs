use std::fmt;

use url::Url;

use crate::{
    key::asset_root_for_url,
    naming::{safe_path_component, url_fingerprint},
};

struct Consts;
impl Consts {
    const MAX_EXTENSION_LEN: usize = 16;
    const MAX_LEAF_COMPONENT_LEN: usize = 96;
}

/// On-disk layout policy carried by an [`AssetScope`](crate::AssetScope):
/// maps a resource URL to a relative path inside the asset's directory.
pub trait AssetLayout: fmt::Debug + Send + Sync {
    /// Derive the on-disk asset directory name for a source URL.
    #[must_use]
    fn asset_root(&self, url: &Url, name: Option<&str>) -> String {
        asset_root_for_url(url, name)
    }

    /// Derive the relative path for the resource at `url` inside the scope's asset root.
    #[must_use]
    fn rel_path(&self, url: &Url) -> String;
}

/// Fingerprinted URL mirror: `hls/<host>/<path>/<leaf>_<fingerprint>.<ext>`.
/// The `hls/` prefix is a fixed literal; on-disk caches address it byte-for-byte.
#[derive(Debug, Default)]
pub struct DefaultLayout;

impl AssetLayout for DefaultLayout {
    fn rel_path(&self, url: &Url) -> String {
        url_mirror(url)
    }
}

fn url_mirror(url: &Url) -> String {
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

    fn url(s: &str) -> Url {
        Url::parse(s).expect("valid test URL")
    }

    // Golden literals (fingerprint included): a drift in url_fingerprint or
    // safe_path_component silently orphans existing caches.
    #[kithara::test]
    fn default_manifest_mirrors_url_tree_without_query() {
        let u = url("https://cdn.example.com/audio/v0/index.m3u8?token=secret");
        assert_eq!(
            DefaultLayout.rel_path(&u),
            "hls/cdn.example.com/audio/v0/index_7d31050f3d8af153.m3u8"
        );
    }

    #[kithara::test]
    fn default_segment_and_key_share_the_mirror() {
        let seg = url("https://cdn.example.com/a/seg001.m4s?X-Amz-Signature=abc");
        assert_eq!(
            DefaultLayout.rel_path(&seg),
            "hls/cdn.example.com/a/seg001_b121e4c47d79143a.m4s"
        );

        let key = url("https://cdn.example.com/a/key.bin");
        assert_eq!(
            DefaultLayout.rel_path(&key),
            "hls/cdn.example.com/a/key_5195634fe6e6cb0b.bin"
        );
    }

    #[kithara::test]
    fn default_track_shares_the_mirror() {
        let u = url("https://example.com/get-mp3/42?sign=x");
        assert_eq!(
            DefaultLayout.rel_path(&u),
            format!("hls/example.com/get-mp3/42_{}", url_fingerprint(&u))
        );
    }
}
