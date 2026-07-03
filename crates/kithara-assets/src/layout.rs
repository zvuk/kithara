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
    const SEGMENT_INDEX_WIDTH: usize = 5;
}

/// Reserved label for the master manifest (`PrettyLayout`). A variant label
/// equal to this is disambiguated so it cannot mint the master's own key.
const MASTER_STEM: &str = "master";

/// On-disk layout policy carried by an [`AssetScope`](crate::AssetScope).
///
/// Maps a [`ResourceInfo`] to a relative path inside the asset's directory.
/// `asset_root` stays layout-independent so switching layouts over an existing cache keeps the same asset directory.
pub trait AssetLayout: fmt::Debug + Send + Sync {
    /// Derive the on-disk asset directory name for a source URL.
    #[must_use]
    fn asset_root(&self, url: &Url, name: Option<&str>) -> String {
        asset_root_for_url(url, name)
    }

    /// Derive the relative path for `info` inside the scope's asset root.
    #[must_use]
    fn rel_path(&self, info: &ResourceInfo<'_>) -> String;
}

/// One rendition's stable facts, as parsed from the master playlist.
/// Owned so the whole sibling set can travel by reference in a [`RenditionDesc`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rendition {
    /// Advertised bandwidth in bits per second, when present.
    pub bandwidth: Option<u64>,
    /// `NAME` attribute, when present.
    pub name: Option<String>,
    /// Stem of the variant playlist URI (leaf filename without extension),
    /// when the master advertises a meaningful one.
    pub uri_stem: Option<String>,
}

impl Rendition {
    /// Construct a rendition from its advertised bandwidth, name, and
    /// variant-playlist URI stem.
    #[must_use]
    pub fn new(bandwidth: Option<u64>, name: Option<String>, uri_stem: Option<String>) -> Self {
        Self {
            bandwidth,
            name,
            uri_stem,
        }
    }
}

/// A single rendition selected from the master's full rendition set.
///
/// Carries the whole sibling set so a layout can derive labels (rank,
/// name-uniqueness, dedup) as a pure function of the master alone.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct RenditionDesc<'a> {
    /// Position of this rendition in master-playlist order (stable).
    pub idx: usize,
    /// Every rendition in the master, in master order.
    pub siblings: &'a [Rendition],
}

impl<'a> RenditionDesc<'a> {
    /// Construct a descriptor selecting rendition `idx` from `siblings`.
    #[must_use]
    pub fn new(idx: usize, siblings: &'a [Rendition]) -> Self {
        Self { idx, siblings }
    }
}

/// Description of a single fetched resource. The layout maps it to a relative path.
/// The vocabulary is adaptive-streaming (manifest / rendition / segment / key / track).
#[non_exhaustive]
pub enum ResourceInfo<'a> {
    /// Playlist/manifest; `rendition: None` is the master.
    Manifest {
        url: &'a Url,
        rendition: Option<RenditionDesc<'a>>,
    },
    /// Rendition initialization segment (fMP4 `EXT-X-MAP`).
    InitSegment {
        url: &'a Url,
        rendition: RenditionDesc<'a>,
    },
    /// One media segment of a rendition.
    MediaSegment {
        url: &'a Url,
        rendition: RenditionDesc<'a>,
        /// Media-sequence number (`EXT-X-MEDIA-SEQUENCE` + playlist position),
        /// stable across live playlist reloads.
        sequence: u64,
    },
    /// Decryption key.
    Key { url: &'a Url },
    /// Monolithic remote file (MP3 and friends).
    Track {
        url: &'a Url,
        name: Option<&'a str>,
        ext_hint: Option<&'a str>,
    },
}

impl ResourceInfo<'_> {
    /// The source URL of this resource.
    #[must_use]
    pub fn url(&self) -> &Url {
        match self {
            Self::Manifest { url, .. }
            | Self::InitSegment { url, .. }
            | Self::MediaSegment { url, .. }
            | Self::Key { url }
            | Self::Track { url, .. } => url,
        }
    }
}

/// The URL url-mirror for every streaming resource and `track.<ext>` for a monolithic file.
#[derive(Debug, Default)]
pub struct DefaultLayout;

impl AssetLayout for DefaultLayout {
    fn rel_path(&self, info: &ResourceInfo<'_>) -> String {
        match info {
            ResourceInfo::Track { ext_hint, .. } => track_rel_path(*ext_hint),
            _ => hls_url_mirror(info.url()),
        }
    }
}

/// Semantic naming: `master.m3u8`, `<label>.m3u8` (variant playlists sit
/// next to the master), `<label>/init.<ext>`, `<label>/seg_<seq:05>.<ext>`,
/// `keys/<fingerprint>.key`, and `<safe(name | leaf stem)>.<ext>` for a
/// monolithic file. `<label>` is derived from the master's rendition set.
#[derive(Debug, Default)]
pub struct PrettyLayout;

impl AssetLayout for PrettyLayout {
    fn rel_path(&self, info: &ResourceInfo<'_>) -> String {
        match info {
            ResourceInfo::Manifest {
                rendition: None, ..
            } => format!("{MASTER_STEM}.m3u8"),
            ResourceInfo::Manifest {
                url,
                rendition: Some(desc),
            } => format!("{}{}", label_for(desc), playlist_ext(url)),
            ResourceInfo::InitSegment { url, rendition } => {
                format!("{}/init{}", label_for(rendition), segment_ext(url, ".mp4"))
            }
            ResourceInfo::MediaSegment {
                url,
                rendition,
                sequence,
            } => format!(
                "{}/seg_{:0width$}{}",
                label_for(rendition),
                sequence,
                segment_ext(url, ".m4s"),
                width = Consts::SEGMENT_INDEX_WIDTH,
            ),
            ResourceInfo::Key { url } => format!("keys/{}.key", url_fingerprint(url)),
            ResourceInfo::Track {
                url,
                name,
                ext_hint,
            } => pretty_track_rel_path(url, *name, *ext_hint),
        }
    }
}

fn track_rel_path(ext_hint: Option<&str>) -> String {
    ext_hint.map_or_else(
        || "track".to_string(),
        |ext| format!("track.{}", safe_path_component(ext, ext)),
    )
}

fn pretty_track_rel_path(url: &Url, name: Option<&str>, ext_hint: Option<&str>) -> String {
    let leaf = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty());
    let stem_source = name
        .filter(|value| !value.is_empty())
        .or_else(|| leaf.map(leaf_stem))
        .unwrap_or("track");
    let identity = url.as_str();
    let stem = safe_path_component(stem_source, identity);
    let ext = ext_hint
        .or_else(|| leaf.and_then(leaf_ext))
        .map(|ext| extension_component(ext, identity));
    match ext {
        Some(ext) if !ext.is_empty() => format!("{stem}.{ext}"),
        _ => stem,
    }
}

fn leaf_stem(leaf: &str) -> &str {
    match leaf.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && is_ext(ext) => stem,
        _ => leaf,
    }
}

fn leaf_ext(leaf: &str) -> Option<&str> {
    match leaf.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && is_ext(ext) => Some(ext),
        _ => None,
    }
}

fn is_ext(ext: &str) -> bool {
    !ext.is_empty() && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn label_for(desc: &RenditionDesc<'_>) -> String {
    let labels = derive_labels(desc.siblings);
    labels
        .get(desc.idx)
        .cloned()
        .unwrap_or_else(|| format!("q{}", desc.idx.saturating_add(1)))
}

#[must_use]
fn derive_labels(renditions: &[Rendition]) -> Vec<String> {
    let base = if all_unique_by(renditions, |r| r.name.as_deref()) {
        labels_by_key(renditions, |r| r.name.as_deref())
    } else if all_unique_by(renditions, |r| r.uri_stem.as_deref()) {
        labels_by_key(renditions, |r| r.uri_stem.as_deref())
    } else {
        labels_by_bandwidth(renditions)
    };
    dedup_labels(base)
}

/// True when every rendition has a non-empty key and all keys are distinct.
fn all_unique_by(renditions: &[Rendition], key: impl Fn(&Rendition) -> Option<&str>) -> bool {
    if renditions.is_empty() {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    for rendition in renditions {
        match key(rendition).filter(|value| !value.is_empty()) {
            Some(value) if seen.insert(value) => {}
            _ => return false,
        }
    }
    true
}

fn labels_by_key(
    renditions: &[Rendition],
    key: impl Fn(&Rendition) -> Option<&str>,
) -> Vec<String> {
    renditions
        .iter()
        .map(|r| {
            key(r).map_or_else(
                || "media".to_string(),
                |value| safe_path_component(value, value),
            )
        })
        .collect()
}

fn labels_by_bandwidth(renditions: &[Rendition]) -> Vec<String> {
    let count = renditions.len();
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&i| (renditions[i].bandwidth.unwrap_or(0), i));

    let mut labels = vec![String::new(); count];
    for (rank, &idx) in order.iter().enumerate() {
        labels[idx] = rank_label(rank, count, renditions[idx].bandwidth);
    }
    labels
}

fn rank_label(rank: usize, count: usize, bandwidth: Option<u64>) -> String {
    match count {
        0 | 1 => "media".to_string(),
        2 => match rank {
            0 => "low".to_string(),
            _ => "high".to_string(),
        },
        3 => match rank {
            0 => "low".to_string(),
            1 => "mid".to_string(),
            _ => "high".to_string(),
        },
        _ => {
            let kbps = bandwidth.unwrap_or(0) / 1000;
            format!("q{}_{}kbps", rank.saturating_add(1), kbps)
        }
    }
}

fn dedup_labels(labels: Vec<String>) -> Vec<String> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for label in &labels {
        *counts.entry(label.as_str()).or_insert(0) += 1;
    }
    let duplicated: std::collections::HashSet<String> = counts
        .into_iter()
        .filter(|&(_, n)| n > 1)
        .map(|(label, _)| label.to_string())
        .collect();
    labels
        .into_iter()
        .enumerate()
        .map(|(idx, label)| {
            // `master` is reserved: a variant labelled `master` would mint the
            // flat `master.m3u8`, colliding with the master manifest's own key.
            if duplicated.contains(&label) || label == MASTER_STEM {
                format!("{label}_{idx}")
            } else {
                label
            }
        })
        .collect()
}

fn playlist_ext(url: &Url) -> String {
    segment_ext(url, ".m3u8")
}

fn segment_ext(url: &Url, default: &str) -> String {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(leaf_ext)
        .map_or_else(
            || default.to_string(),
            |ext| format!(".{}", extension_component(ext, url.as_str())),
        )
}

fn hls_url_mirror(url: &Url) -> String {
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

    fn renditions(specs: &[(Option<u64>, Option<&str>)]) -> Vec<Rendition> {
        specs
            .iter()
            .map(|(bw, name)| Rendition::new(*bw, name.map(str::to_string), None))
            .collect()
    }

    fn renditions_with_stems(specs: &[(Option<u64>, Option<&str>)]) -> Vec<Rendition> {
        specs
            .iter()
            .map(|(bw, stem)| Rendition::new(*bw, None, stem.map(str::to_string)))
            .collect()
    }

    // Golden literals (fingerprint included): a drift in url_fingerprint or
    // safe_path_component silently orphans existing caches.
    #[kithara::test]
    fn default_hls_manifest_mirrors_url_tree_without_query() {
        let u = url("https://cdn.example.com/audio/v0/index.m3u8?token=secret");
        let rel = DefaultLayout.rel_path(&ResourceInfo::Manifest {
            url: &u,
            rendition: None,
        });
        assert_eq!(
            rel,
            "hls/cdn.example.com/audio/v0/index_7d31050f3d8af153.m3u8"
        );
    }

    #[kithara::test]
    fn default_hls_segment_and_key_share_the_mirror() {
        let sibs = renditions(&[(Some(1_000), None)]);
        let seg = url("https://cdn.example.com/a/seg001.m4s?X-Amz-Signature=abc");
        let rel = DefaultLayout.rel_path(&ResourceInfo::MediaSegment {
            url: &seg,
            rendition: RenditionDesc::new(0, &sibs),
            sequence: 0,
        });
        assert_eq!(rel, "hls/cdn.example.com/a/seg001_b121e4c47d79143a.m4s");

        let key = url("https://cdn.example.com/a/key.bin");
        let key_rel = DefaultLayout.rel_path(&ResourceInfo::Key { url: &key });
        assert_eq!(key_rel, "hls/cdn.example.com/a/key_5195634fe6e6cb0b.bin");
    }

    #[kithara::test]
    fn default_track_uses_track_dot_ext() {
        let u = url("https://example.com/get-mp3/42?sign=x");
        assert_eq!(
            DefaultLayout.rel_path(&ResourceInfo::Track {
                url: &u,
                name: None,
                ext_hint: Some("mp3"),
            }),
            "track.mp3"
        );
        assert_eq!(
            DefaultLayout.rel_path(&ResourceInfo::Track {
                url: &u,
                name: None,
                ext_hint: None,
            }),
            "track"
        );
    }

    #[kithara::test]
    fn pretty_master_is_master_m3u8() {
        let u = url("https://x/master.m3u8");
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::Manifest {
                url: &u,
                rendition: None
            }),
            "master.m3u8"
        );
    }

    #[kithara::test]
    fn pretty_two_variant_tree_is_low_high() {
        let sibs = renditions(&[(Some(1_280_000), None), (Some(5_120_000), None)]);
        let media = url("https://x/v0.m3u8");
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::Manifest {
                url: &media,
                rendition: Some(RenditionDesc::new(0, &sibs)),
            }),
            "low.m3u8",
            "variant playlist sits flat next to the master"
        );
        let init = url("https://x/init.mp4");
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::InitSegment {
                url: &init,
                rendition: RenditionDesc::new(1, &sibs),
            }),
            "high/init.mp4"
        );
        let seg = url("https://x/seg3.m4s");
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::MediaSegment {
                url: &seg,
                rendition: RenditionDesc::new(1, &sibs),
                sequence: 3,
            }),
            "high/seg_00003.m4s"
        );
    }

    #[kithara::test]
    fn pretty_labels_prefer_unique_uri_stems_over_bandwidth() {
        // No NAME, but distinct playlist stems -> stems win over bandwidth rank.
        let sibs = renditions_with_stems(&[
            (Some(66_005), Some("index-slq-a1")),
            (Some(988_758), Some("index-slossless-a1")),
        ]);
        let media = url("https://x/index-slq-a1.m3u8");
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::Manifest {
                url: &media,
                rendition: Some(RenditionDesc::new(0, &sibs)),
            }),
            "index-slq-a1.m3u8"
        );
        let seg = url("https://x/index-slossless-a1/seg0.m4s");
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::MediaSegment {
                url: &seg,
                rendition: RenditionDesc::new(1, &sibs),
                sequence: 7,
            }),
            "index-slossless-a1/seg_00007.m4s"
        );
    }

    #[kithara::test]
    fn pretty_variant_labelled_master_never_collides_with_master_manifest() {
        // A variant whose URI stem is `master` must not mint the flat
        // `master.m3u8` used by the master manifest itself.
        let sibs =
            renditions_with_stems(&[(Some(1_000), Some("master")), (Some(2_000), Some("hi"))]);
        let media = url("https://x/master.m3u8");
        let variant = PrettyLayout.rel_path(&ResourceInfo::Manifest {
            url: &media,
            rendition: Some(RenditionDesc::new(0, &sibs)),
        });
        let master = PrettyLayout.rel_path(&ResourceInfo::Manifest {
            url: &media,
            rendition: None,
        });
        assert_eq!(master, "master.m3u8");
        assert_ne!(variant, master, "variant must not shadow the master key");
        assert_eq!(variant, "master_0.m3u8");
    }

    #[kithara::test]
    fn labels_dupe_uri_stems_fall_back_to_bandwidth() {
        // Every variant advertises the same opaque `chunklist` stem: not
        // unique, so labels come from bandwidth rank.
        let labels = derive_labels(&renditions_with_stems(&[
            (Some(1_000), Some("chunklist")),
            (Some(2_000), Some("chunklist")),
        ]));
        assert_eq!(labels, ["low", "high"]);
    }

    #[kithara::test]
    fn pretty_key_is_fingerprinted_under_keys() {
        let u = url("https://x/aes/key.bin");
        let rel = PrettyLayout.rel_path(&ResourceInfo::Key { url: &u });
        assert_eq!(rel, "keys/a0fc23a0c9505d7d.key");
    }

    #[kithara::test]
    fn pretty_track_uses_named_file() {
        let u = url("https://example.com/audio/song.mp3?sig=x");
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::Track {
                url: &u,
                name: Some("My Track"),
                ext_hint: Some("mp3"),
            }),
            "My_Track.mp3"
        );
        assert_eq!(
            PrettyLayout.rel_path(&ResourceInfo::Track {
                url: &u,
                name: None,
                ext_hint: None,
            }),
            "song.mp3",
            "no explicit name falls back to the URL leaf stem"
        );
    }

    #[kithara::test]
    fn labels_single_variant_is_media() {
        assert_eq!(
            derive_labels(&renditions(&[(Some(1_000), None)])),
            ["media"]
        );
    }

    #[kithara::test]
    fn labels_two_and_three_are_ranked() {
        assert_eq!(
            derive_labels(&renditions(&[(Some(2_000), None), (Some(1_000), None)])),
            ["high", "low"],
            "rank follows bandwidth, not master order"
        );
        assert_eq!(
            derive_labels(&renditions(&[
                (Some(1_000), None),
                (Some(3_000), None),
                (Some(2_000), None),
            ])),
            ["low", "high", "mid"],
        );
    }

    #[kithara::test]
    fn labels_four_use_qrank_kbps() {
        let labels = derive_labels(&renditions(&[
            (Some(1_000_000), None),
            (Some(2_000_000), None),
            (Some(3_000_000), None),
            (Some(4_000_000), None),
        ]));
        assert_eq!(labels[0], "q1_1000kbps");
        assert_eq!(labels[3], "q4_4000kbps");
    }

    #[kithara::test]
    fn labels_prefer_unique_names() {
        assert_eq!(
            derive_labels(&renditions(&[
                (Some(1_000), Some("Lo-Fi")),
                (Some(2_000), Some("Hi-Fi")),
            ])),
            ["Lo-Fi", "Hi-Fi"],
        );
    }

    #[kithara::test]
    fn labels_dupe_names_fall_back_to_bandwidth_rank() {
        // Duplicate NAME is not unique, so labels come from bandwidth rank.
        let labels = derive_labels(&renditions(&[
            (Some(1_000), Some("Audio")),
            (Some(1_000), Some("Audio")),
        ]));
        assert_eq!(labels, ["low", "high"]);
    }

    #[kithara::test]
    fn labels_dedup_distinct_names_that_sanitize_alike() {
        // Two distinct NAMEs collapsing to the same safe component are the
        // only real dedup trigger: they get `_<idx>` suffixes.
        let labels = derive_labels(&renditions(&[
            (Some(1_000), Some("a/b")),
            (Some(2_000), Some("a\\b")),
        ]));
        assert_eq!(labels.len(), 2);
        assert_ne!(
            labels[0], labels[1],
            "distinct renditions must map to distinct folders"
        );
    }

    #[kithara::test]
    fn labels_missing_bandwidth_is_deterministic() {
        let labels = derive_labels(&renditions(&[(None, None), (Some(1_000), None)]));
        assert_eq!(labels, ["low", "high"]);
    }
}
