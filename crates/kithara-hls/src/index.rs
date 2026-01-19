//! Segment index for random-access over HLS streams.
//!
//! Multi-variant index: each variant has its own segment index with independent byte offsets.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
};

use url::Url;

/// Detect container format from segment URL extension.
fn detect_container_from_url(url: &Url) -> Option<DetectedContainer> {
    let path = url.path();
    let ext = path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "ts" | "m2ts" => Some(DetectedContainer::MpegTs),
        "mp4" | "m4s" | "m4a" | "m4v" => Some(DetectedContainer::Fmp4),
        _ => None,
    }
}

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
    /// Base byte offset for this variant (for ABR switch continuity).
    /// When switching to this variant from another, base_offset is set to
    /// the last read position, ensuring continuous global offset namespace.
    base_offset: u64,
    /// Container format detected from init/first segment URL.
    detected_container: Option<DetectedContainer>,
    /// First media segment index (for ABR switch support).
    /// When ABR switches mid-stream, we start from segment N, not 0.
    first_media_segment: Option<usize>,
}

impl VariantIndex {
    fn new() -> Self {
        Self {
            segments: BTreeMap::new(),
            base_offset: 0,
            detected_container: None,
            first_media_segment: None,
        }
    }

    fn add(
        &mut self,
        url: Url,
        len: u64,
        segment_index: usize,
        encryption: Option<EncryptionInfo>,
    ) {
        let key = if segment_index == usize::MAX {
            SegmentKey::Init
        } else {
            // Track first media segment for ABR switch support
            if self.first_media_segment.is_none() {
                self.first_media_segment = Some(segment_index);
            }
            SegmentKey::Media(segment_index)
        };

        // Detect container from init segment or first media segment
        if self.detected_container.is_none()
            && (key == SegmentKey::Init || self.first_media_segment == Some(segment_index))
        {
            self.detected_container = detect_container_from_url(&url);
        }

        self.segments.insert(
            key,
            SegmentData {
                url,
                len,
                encryption,
            },
        );
    }

    fn detected_container(&self) -> Option<DetectedContainer> {
        self.detected_container
    }

    /// Find segment by byte offset (local to this variant).
    /// INIT segment (if present) comes first at offset 0.
    /// Returns None if there are gaps in media segment indices.
    ///
    /// Returns SegmentEntry with local offsets (caller must add base_offset).
    fn find(&self, local_offset: u64) -> Option<SegmentEntry> {
        let mut cumulative = 0u64;
        // Start from first_media_segment (supports ABR switch mid-stream)
        let mut expected_media_idx = self.first_media_segment.unwrap_or(0);

        for (key, data) in &self.segments {
            // Gap detection for media segments.
            if let SegmentKey::Media(idx) = key {
                if *idx > expected_media_idx {
                    return None;
                }
                expected_media_idx = idx + 1;
            }

            let local_start = cumulative;
            let local_end = cumulative + data.len;

            if local_offset >= local_start && local_offset < local_end {
                let segment_index = match key {
                    SegmentKey::Init => usize::MAX,
                    SegmentKey::Media(idx) => *idx,
                };
                return Some(SegmentEntry {
                    global_start: local_start,
                    global_end: local_end,
                    url: data.url.clone(),
                    segment_index,
                    encryption: data.encryption.clone(),
                });
            }
            cumulative = local_end;
        }
        None
    }

    fn total_len(&self) -> u64 {
        self.segments.values().map(|d| d.len).sum()
    }
}

/// Container format detected from segment URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedContainer {
    MpegTs,
    Fmp4,
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

    /// Set base offset for a variant.
    ///
    /// Used during ABR switch to ensure continuous global offset namespace.
    /// When switching from variant A to variant B, call this with the last
    /// read position in variant A.
    pub fn set_variant_base_offset(&mut self, variant: usize, base_offset: u64) {
        if let Some(variant_idx) = self.variants.get_mut(&variant) {
            tracing::debug!(
                variant,
                old_base = variant_idx.base_offset,
                new_base = base_offset,
                "Setting variant base offset"
            );
            variant_idx.base_offset = base_offset;
        }
    }

    /// Add a segment to the variant's index.
    ///
    /// If this is the first segment for a NEW variant (ABR switch),
    /// automatically sets base_offset to ensure continuous global offset space.
    pub fn add(
        &mut self,
        url: Url,
        len: u64,
        variant: usize,
        segment_index: usize,
        encryption: Option<EncryptionInfo>,
    ) {
        // Check if this is a new variant
        let is_new_variant = !self.variants.contains_key(&variant);

        // If new variant, calculate max_offset from existing variants BEFORE borrowing
        let base_offset_for_new = if is_new_variant {
            let max_offset = self.variants
                .iter()
                .map(|(_, v)| v.base_offset + v.total_len())
                .max()
                .unwrap_or(0);

            if max_offset > 0 {
                tracing::debug!(
                    variant,
                    base_offset = max_offset,
                    "Auto-setting base offset for new variant (ABR switch)"
                );
                Some(max_offset)
            } else {
                None
            }
        } else {
            None
        };

        let variant_idx = self.variants
            .entry(variant)
            .or_insert_with(VariantIndex::new);

        // Set base offset for new variant
        if let Some(base) = base_offset_for_new {
            variant_idx.base_offset = base;
        }

        variant_idx.add(url, len, segment_index, encryption);
    }

    /// Get detected container for a variant (if available).
    pub fn detected_container(&self, variant: usize) -> Option<DetectedContainer> {
        self.variants
            .get(&variant)
            .and_then(|v| v.detected_container())
    }

    /// Find segment by offset for the specified variant.
    /// Returns owned SegmentEntry (computed on the fly).
    ///
    /// Handles variant base offsets for ABR switch continuity:
    /// - Converts global offset to variant-local offset
    /// - Finds segment in variant's index
    /// - Converts result back to global offset space
    pub fn find(&self, offset: u64, variant: usize) -> Option<SegmentEntry> {
        let variant_idx = self.variants.get(&variant)?;
        let base_offset = variant_idx.base_offset;
        let local_offset = offset.saturating_sub(base_offset);

        let mut entry = variant_idx.find(local_offset)?;

        // Adjust global offsets by adding base offset
        entry.global_start = entry.global_start.saturating_add(base_offset);
        entry.global_end = entry.global_end.saturating_add(base_offset);

        Some(entry)
    }

    /// Find segment_index by offset in any loaded variant.
    /// Used as a hint for seek when the current variant doesn't have the needed segment.
    /// Returns None for init segments (usize::MAX) - they are loaded automatically on variant switch.
    pub fn find_segment_index_for_offset(&self, offset: u64) -> Option<usize> {
        for variant_idx in self.variants.values() {
            if let Some(entry) = variant_idx.find(offset) {
                // Don't return init segment index - init is handled by variant switch
                if entry.segment_index == usize::MAX {
                    continue;
                }
                return Some(entry.segment_index);
            }
        }
        None
    }

    /// Total length for the specified variant (or 0 if variant not loaded).
    ///
    /// Returns base_offset + variant's local total length, ensuring continuous
    /// global offset space across ABR switches.
    pub fn total_len(&self, variant: usize) -> u64 {
        self.variants.get(&variant).map_or(0, |v| {
            v.base_offset.saturating_add(v.total_len())
        })
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
