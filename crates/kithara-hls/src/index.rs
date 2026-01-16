//! Segment index for random-access over HLS streams.
//!
//! Multi-variant index: each variant has its own segment index with independent byte offsets.

use std::{cmp::Ordering, collections::{BTreeMap, HashMap}};

use url::Url;

/// Encryption info for a segment (resolved key URL and IV).
/// Also used as context for decryption callback in `AssetStore`.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct EncryptionInfo {
    pub key_url: Url,
    pub iv: [u8; 16],
}

/// Key for segment storage in BTreeMap.
/// Ordering: Init < Media(0) < Media(1) < ...
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentKey {
    Init,
    Media(usize),
}

impl PartialOrd for SegmentKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SegmentKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SegmentKey::Init, SegmentKey::Init) => Ordering::Equal,
            (SegmentKey::Init, SegmentKey::Media(_)) => Ordering::Less,
            (SegmentKey::Media(_), SegmentKey::Init) => Ordering::Greater,
            (SegmentKey::Media(a), SegmentKey::Media(b)) => a.cmp(b),
        }
    }
}

/// Internal segment data stored in the index.
#[derive(Debug, Clone)]
struct SegmentData {
    url: Url,
    len: u64,
    encryption: Option<EncryptionInfo>,
}

/// Entry in segment index: maps global byte range to segment file.
#[derive(Debug, Clone)]
pub(crate) struct SegmentEntry {
    pub global_start: u64,
    pub global_end: u64,
    pub url: Url,
    pub segment_index: usize,
    pub encryption: Option<EncryptionInfo>,
}

/// Index for a single variant.
/// Segments stored in BTreeMap with SegmentKey ordering: Init < Media(0) < Media(1) < ...
struct VariantIndex {
    segments: BTreeMap<SegmentKey, SegmentData>,
}

impl VariantIndex {
    fn new() -> Self {
        Self {
            segments: BTreeMap::new(),
        }
    }

    fn add(&mut self, url: Url, len: u64, segment_index: usize, encryption: Option<EncryptionInfo>) {
        let key = if segment_index == usize::MAX {
            SegmentKey::Init
        } else {
            SegmentKey::Media(segment_index)
        };
        self.segments.insert(key, SegmentData { url, len, encryption });
    }

    /// Find segment by byte offset.
    /// INIT segment (if present) comes first at offset 0.
    /// Returns None if there are gaps in media segment indices.
    fn find(&self, offset: u64) -> Option<SegmentEntry> {
        let mut cumulative = 0u64;
        let mut expected_media_idx = 0usize;

        for (key, data) in &self.segments {
            // Gap detection for media segments.
            if let SegmentKey::Media(idx) = key {
                if *idx > expected_media_idx {
                    return None;
                }
                expected_media_idx = idx + 1;
            }

            let global_start = cumulative;
            let global_end = cumulative + data.len;

            if offset >= global_start && offset < global_end {
                let segment_index = match key {
                    SegmentKey::Init => usize::MAX,
                    SegmentKey::Media(idx) => *idx,
                };
                return Some(SegmentEntry {
                    global_start,
                    global_end,
                    url: data.url.clone(),
                    segment_index,
                    encryption: data.encryption.clone(),
                });
            }
            cumulative = global_end;
        }
        None
    }

    fn total_len(&self) -> u64 {
        self.segments.values().map(|d| d.len).sum()
    }
}

/// Multi-variant segment index.
pub(crate) struct SegmentIndex {
    variants: HashMap<usize, VariantIndex>,
    finished: bool,
    error: Option<String>,
}

impl SegmentIndex {
    pub fn new() -> Self {
        Self {
            variants: HashMap::new(),
            finished: false,
            error: None,
        }
    }

    /// Add a segment to the variant's index.
    pub fn add(
        &mut self,
        url: Url,
        len: u64,
        variant: usize,
        segment_index: usize,
        encryption: Option<EncryptionInfo>,
    ) {
        self.variants
            .entry(variant)
            .or_insert_with(VariantIndex::new)
            .add(url, len, segment_index, encryption);
    }

    /// Find segment by offset for the specified variant.
    /// Returns owned SegmentEntry (computed on the fly).
    pub fn find(&self, offset: u64, variant: usize) -> Option<SegmentEntry> {
        self.variants.get(&variant)?.find(offset)
    }

    /// Find segment_index by offset in any loaded variant.
    /// Used as a hint for seek when the current variant doesn't have the needed segment.
    pub fn find_segment_index_for_offset(&self, offset: u64) -> Option<usize> {
        for variant_idx in self.variants.values() {
            if let Some(entry) = variant_idx.find(offset) {
                return Some(entry.segment_index);
            }
        }
        None
    }

    /// Total length for the specified variant (or 0 if variant not loaded).
    pub fn total_len(&self, variant: usize) -> u64 {
        self.variants.get(&variant).map_or(0, |v| v.total_len())
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn set_finished(&mut self) {
        self.finished = true;
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.finished = true;
    }
}
