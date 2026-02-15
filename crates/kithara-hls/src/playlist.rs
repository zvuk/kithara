//! Playlist state: single source of truth for parsed HLS playlist data.
//!
//! `PlaylistState` holds all variant and segment information parsed from
//! master and media playlists, with per-variant size maps for byte-offset
//! resolution. The `PlaylistAccess` trait provides a testable read interface.

use std::time::Duration;

use kithara_stream::{AudioCodec, ContainerFormat};
use parking_lot::RwLock;
use url::Url;

use crate::parsing::SegmentKey;

// ---- Types ----

/// Per-segment parsed data (from media playlist).
#[derive(Debug, Clone)]
pub struct SegmentState {
    /// Segment index within the variant's media playlist.
    pub index: usize,
    /// Absolute URL of the segment.
    pub url: Url,
    /// Duration of the segment.
    pub duration: Duration,
    /// Encryption key for this segment (if encrypted).
    pub key: Option<SegmentKey>,
}

/// Per-variant size information (from HEAD requests or download).
///
/// Uses a 0-based offset model: the first segment starts at offset 0 and
/// includes init data. `segment_sizes[0]` = `init_size` + `media_len_0`.
#[derive(Debug, Clone)]
pub struct VariantSizeMap {
    /// Size of the init segment in bytes.
    pub init_size: u64,
    /// Per-segment total sizes in bytes. `segment_sizes[0]` includes `init_size`.
    pub segment_sizes: Vec<u64>,
    /// Cumulative byte offsets. `offsets[0]` = 0, `offsets[i]` = sum of `segment_sizes[0..i]`.
    pub offsets: Vec<u64>,
    /// Total size of the variant (init + all segments).
    pub total: u64,
}

/// Per-variant parsed data.
#[derive(Debug)]
pub struct VariantState {
    /// Variant index in the master playlist.
    pub id: usize,
    /// Absolute URL of the variant's media playlist.
    pub uri: Url,
    /// Advertised bandwidth in bits per second.
    pub bandwidth: Option<u64>,
    /// Audio codec (parsed from CODECS attribute).
    pub codec: Option<AudioCodec>,
    /// Container format (detected from segment URIs).
    pub container: Option<ContainerFormat>,
    /// Absolute URL of the init segment (fMP4 only).
    pub init_url: Option<Url>,
    /// Parsed segments from the media playlist.
    pub segments: Vec<SegmentState>,
    /// Size map (populated after HEAD requests or first download).
    pub size_map: Option<VariantSizeMap>,
}

// ---- PlaylistState ----

/// Holds all parsed playlist data with interior mutability for size map updates.
pub struct PlaylistState {
    variants: Vec<RwLock<VariantState>>,
}

impl PlaylistState {
    /// Create a new playlist state from parsed variants.
    pub fn new(variants: Vec<VariantState>) -> Self {
        Self {
            variants: variants.into_iter().map(RwLock::new).collect(),
        }
    }

    /// Build from parsed playlists (master variants + media playlists).
    ///
    /// Each entry in `media_playlists` corresponds to the variant at the same
    /// index in `variants`. Segment URIs are resolved against the media
    /// playlist URL; failures fall back to the media URL itself.
    pub fn from_parsed(
        variants: &[crate::parsing::VariantStream],
        media_playlists: &[(Url, crate::parsing::MediaPlaylist)],
    ) -> Self {
        let variant_states: Vec<VariantState> = variants
            .iter()
            .zip(media_playlists.iter())
            .map(|(variant, (media_url, playlist))| {
                // Resolve codec and container from variant metadata
                let codec = variant.codec.as_ref().and_then(|ci| ci.audio_codec);
                let container = variant
                    .codec
                    .as_ref()
                    .and_then(|ci| ci.container)
                    .or(playlist.detected_container);

                // Resolve init segment URL
                let init_url = playlist
                    .init_segment
                    .as_ref()
                    .and_then(|init| media_url.join(&init.uri).ok());

                // Build segment states
                let segments: Vec<SegmentState> = playlist
                    .segments
                    .iter()
                    .enumerate()
                    .map(|(index, seg)| {
                        let url = media_url
                            .join(&seg.uri)
                            .unwrap_or_else(|_| media_url.clone());
                        SegmentState {
                            index,
                            url,
                            duration: seg.duration,
                            key: seg.key.clone(),
                        }
                    })
                    .collect();

                VariantState {
                    id: variant.id.0,
                    uri: media_url.clone(),
                    bandwidth: variant.bandwidth,
                    codec,
                    container,
                    init_url,
                    segments,
                    size_map: None,
                }
            })
            .collect();

        Self::new(variant_states)
    }

    /// Set the size map for a variant (after HEAD requests or first download).
    pub fn set_size_map(&self, variant: usize, size_map: VariantSizeMap) {
        if let Some(lock) = self.variants.get(variant) {
            let mut state = lock.write();
            state.size_map = Some(size_map);
        }
    }

    /// Reconcile a segment's actual size after DRM decryption.
    ///
    /// Updates the segment's size and recalculates all subsequent offsets.
    /// This handles the case where decrypted size differs from encrypted size.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "write guard borrows size_map mutably"
    )]
    pub fn reconcile_segment_size(&self, variant: usize, segment_index: usize, actual_total: u64) {
        let Some(lock) = self.variants.get(variant) else {
            return;
        };
        let mut state = lock.write();
        let Some(ref mut size_map) = state.size_map else {
            return;
        };
        if segment_index >= size_map.segment_sizes.len() {
            return;
        }

        size_map.segment_sizes[segment_index] = actual_total;

        // Recalculate offsets from next segment onward.
        // offsets[0] is always 0 (first segment starts at the beginning of the virtual stream).
        // offsets[i] = offsets[i-1] + segment_sizes[i-1]
        for i in (segment_index + 1)..size_map.offsets.len() {
            size_map.offsets[i] = size_map.offsets[i - 1] + size_map.segment_sizes[i - 1];
        }

        // Recalculate total.
        let last_idx = size_map.segment_sizes.len() - 1;
        size_map.total = size_map.offsets[last_idx] + size_map.segment_sizes[last_idx];
    }
}

// ---- PlaylistAccess trait ----

/// Read-only access to parsed playlist data.
#[cfg_attr(test, unimock::unimock(api = PlaylistAccessMock))]
pub trait PlaylistAccess: Send + Sync {
    /// Number of variants in the master playlist.
    #[cfg_attr(not(test), expect(dead_code))]
    fn num_variants(&self) -> usize;

    /// Number of segments in a variant's media playlist.
    fn num_segments(&self, variant: usize) -> Option<usize>;

    /// Audio codec for a variant.
    fn variant_codec(&self, variant: usize) -> Option<AudioCodec>;

    /// Container format for a variant.
    fn variant_container(&self, variant: usize) -> Option<ContainerFormat>;

    /// Absolute URL of a specific segment.
    fn segment_url(&self, variant: usize, index: usize) -> Option<Url>;

    /// Absolute URL of the init segment for a variant.
    fn init_url(&self, variant: usize) -> Option<Url>;

    /// Whether the variant has a size map.
    fn has_size_map(&self, variant: usize) -> bool;

    /// Total size of a variant in bytes (init + all segments).
    fn total_variant_size(&self, variant: usize) -> Option<u64>;

    /// Byte offset of a specific segment within the variant's virtual stream.
    fn segment_byte_offset(&self, variant: usize, index: usize) -> Option<u64>;

    /// Find which segment contains the given byte offset (binary search on offsets).
    fn find_segment_at_offset(&self, variant: usize, offset: u64) -> Option<usize>;
}

impl PlaylistAccess for PlaylistState {
    fn num_variants(&self) -> usize {
        self.variants.len()
    }

    fn num_segments(&self, variant: usize) -> Option<usize> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        Some(state.segments.len())
    }

    fn variant_codec(&self, variant: usize) -> Option<AudioCodec> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        state.codec
    }

    fn variant_container(&self, variant: usize) -> Option<ContainerFormat> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        state.container
    }

    fn segment_url(&self, variant: usize, index: usize) -> Option<Url> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        state.segments.get(index).map(|s| s.url.clone())
    }

    fn init_url(&self, variant: usize) -> Option<Url> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        state.init_url.clone()
    }

    fn has_size_map(&self, variant: usize) -> bool {
        let Some(lock) = self.variants.get(variant) else {
            return false;
        };
        let state = lock.read();
        state.size_map.is_some()
    }

    fn total_variant_size(&self, variant: usize) -> Option<u64> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        state.size_map.as_ref().map(|sm| sm.total)
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "size_map borrows the read guard"
    )]
    fn segment_byte_offset(&self, variant: usize, index: usize) -> Option<u64> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        let size_map = state.size_map.as_ref()?;
        size_map.offsets.get(index).copied()
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "size_map borrows the read guard"
    )]
    fn find_segment_at_offset(&self, variant: usize, offset: u64) -> Option<usize> {
        let lock = self.variants.get(variant)?;
        let state = lock.read();
        let size_map = state.size_map.as_ref()?;

        if size_map.offsets.is_empty() || offset >= size_map.total {
            return None;
        }

        // Binary search: find the last offset <= `offset`.
        // offsets is sorted (cumulative), so we search for the rightmost index
        // where offsets[i] <= offset.
        let pos = size_map.offsets.partition_point(|&o| o <= offset);

        // partition_point returns the index of the first element > offset.
        // The segment containing `offset` is at pos - 1.
        // With 0-based model (offsets[0] = 0), pos == 0 can't happen for valid
        // offsets, but guard against it anyway.
        if pos == 0 { None } else { Some(pos - 1) }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use url::Url;

    use super::*;

    fn base_url() -> Url {
        Url::parse("https://cdn.example.com/audio/").unwrap()
    }

    fn make_segment(index: usize) -> SegmentState {
        SegmentState {
            index,
            url: base_url().join(&format!("segment-{index}.m4s")).unwrap(),
            duration: Duration::from_secs(4),
            key: None,
        }
    }

    fn make_variant(id: usize, num_segments: usize) -> VariantState {
        let segments: Vec<SegmentState> = (0..num_segments).map(make_segment).collect();
        VariantState {
            id,
            uri: base_url().join("variant.m3u8").unwrap(),
            bandwidth: Some(128_000),
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            init_url: Some(base_url().join("init.mp4").unwrap()),
            segments,
            size_map: None,
        }
    }

    /// Build a size map using the 0-based model (matching `calculate_size_map`):
    /// - `offsets[0]` = 0
    /// - `segment_sizes[0]` = `init_size` + `media_sizes[0]`
    /// - `segment_sizes[i>0]` = `media_sizes[i]`
    fn make_size_map(init_size: u64, media_sizes: &[u64]) -> VariantSizeMap {
        let mut offsets = Vec::with_capacity(media_sizes.len());
        let mut segment_sizes = Vec::with_capacity(media_sizes.len());
        let mut cumulative = 0u64;
        for (i, &media_size) in media_sizes.iter().enumerate() {
            let total = if i == 0 {
                init_size + media_size
            } else {
                media_size
            };
            offsets.push(cumulative);
            segment_sizes.push(total);
            cumulative += total;
        }
        VariantSizeMap {
            init_size,
            segment_sizes,
            offsets,
            total: cumulative,
        }
    }

    // ---- Test 1: basic access ----

    #[test]
    fn test_playlist_state_basic_access() {
        let state = PlaylistState::new(vec![make_variant(0, 5), make_variant(1, 3)]);

        assert_eq!(state.num_variants(), 2);
        assert_eq!(state.num_segments(0), Some(5));
        assert_eq!(state.num_segments(1), Some(3));

        // Out of bounds
        assert_eq!(state.num_segments(2), None);
        assert_eq!(state.num_segments(99), None);
    }

    // ---- Test 2: variant info ----

    #[test]
    fn test_playlist_state_variant_info() {
        let mut v = make_variant(0, 2);
        v.codec = Some(AudioCodec::Flac);
        v.container = Some(ContainerFormat::Fmp4);
        let state = PlaylistState::new(vec![v]);

        assert_eq!(state.variant_codec(0), Some(AudioCodec::Flac));
        assert_eq!(state.variant_container(0), Some(ContainerFormat::Fmp4));

        // segment_url
        let url = state.segment_url(0, 0).unwrap();
        assert!(url.as_str().contains("segment-0.m4s"));
        let url1 = state.segment_url(0, 1).unwrap();
        assert!(url1.as_str().contains("segment-1.m4s"));
        assert_eq!(state.segment_url(0, 2), None); // out of bounds

        // init_url
        let init = state.init_url(0).unwrap();
        assert!(init.as_str().contains("init.mp4"));

        // out-of-bounds variant
        assert_eq!(state.variant_codec(5), None);
        assert_eq!(state.variant_container(5), None);
        assert_eq!(state.segment_url(5, 0), None);
        assert_eq!(state.init_url(5), None);
    }

    // ---- Test 3: size map not set ----

    #[test]
    fn test_size_map_not_set() {
        let state = PlaylistState::new(vec![make_variant(0, 3)]);

        assert!(!state.has_size_map(0));
        assert_eq!(state.total_variant_size(0), None);
        assert_eq!(state.segment_byte_offset(0, 0), None);
        assert_eq!(state.find_segment_at_offset(0, 0), None);
    }

    // ---- Test 4: size map set and query ----

    #[test]
    fn test_size_map_set_and_query() {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);

        // init=100, media_sizes=[200, 300, 400, 500]
        // segment_sizes = [300, 300, 400, 500]
        // offsets = [0, 300, 600, 1000], total = 1500
        let sm = make_size_map(100, &[200, 300, 400, 500]);
        state.set_size_map(0, sm);

        assert!(state.has_size_map(0));
        assert_eq!(state.total_variant_size(0), Some(1500));

        // Check offsets (0-based: first segment starts at 0)
        assert_eq!(state.segment_byte_offset(0, 0), Some(0));
        assert_eq!(state.segment_byte_offset(0, 1), Some(300));
        assert_eq!(state.segment_byte_offset(0, 2), Some(600));
        assert_eq!(state.segment_byte_offset(0, 3), Some(1000));
        assert_eq!(state.segment_byte_offset(0, 4), None); // out of bounds

        // find_segment_at_offset
        assert_eq!(state.find_segment_at_offset(0, 0), Some(0)); // start of seg 0 (init region)
        assert_eq!(state.find_segment_at_offset(0, 99), Some(0)); // still in seg 0 (init)
        assert_eq!(state.find_segment_at_offset(0, 100), Some(0)); // seg 0 media starts
        assert_eq!(state.find_segment_at_offset(0, 299), Some(0)); // end of seg 0
        assert_eq!(state.find_segment_at_offset(0, 300), Some(1)); // start of seg 1
        assert_eq!(state.find_segment_at_offset(0, 599), Some(1)); // end of seg 1
        assert_eq!(state.find_segment_at_offset(0, 600), Some(2)); // start of seg 2
        assert_eq!(state.find_segment_at_offset(0, 1000), Some(3)); // start of seg 3
        assert_eq!(state.find_segment_at_offset(0, 1499), Some(3)); // last byte
    }

    // ---- Test 5: reconcile segment size ----

    #[test]
    fn test_reconcile_segment_size() {
        let state = PlaylistState::new(vec![make_variant(0, 3)]);

        // init=100, media_sizes=[200, 300, 400]
        // segment_sizes = [300, 300, 400], offsets = [0, 300, 600], total = 1000
        let sm = make_size_map(100, &[200, 300, 400]);
        state.set_size_map(0, sm);

        assert_eq!(state.total_variant_size(0), Some(1000));
        assert_eq!(state.segment_byte_offset(0, 1), Some(300));
        assert_eq!(state.segment_byte_offset(0, 2), Some(600));

        // After DRM decryption, segment 1 is actually 250 bytes (not 300).
        state.reconcile_segment_size(0, 1, 250);

        // offsets recalculated: [0, 300, 550], total = 950
        assert_eq!(state.segment_byte_offset(0, 0), Some(0)); // unchanged
        assert_eq!(state.segment_byte_offset(0, 1), Some(300)); // unchanged
        assert_eq!(state.segment_byte_offset(0, 2), Some(550)); // was 600
        assert_eq!(state.total_variant_size(0), Some(950)); // was 1000

        // Binary search still works after reconciliation
        assert_eq!(state.find_segment_at_offset(0, 549), Some(1));
        assert_eq!(state.find_segment_at_offset(0, 550), Some(2));
    }

    // ---- Test 6: find_segment_at_offset edge cases ----

    #[test]
    fn test_find_segment_at_offset_edge_cases() {
        let state = PlaylistState::new(vec![make_variant(0, 3)]);

        // init=50, media_sizes=[100, 100, 100]
        // segment_sizes = [150, 100, 100], offsets = [0, 150, 250], total = 350
        let sm = make_size_map(50, &[100, 100, 100]);
        state.set_size_map(0, sm);

        // Offset 0 = start of first segment (includes init region)
        assert_eq!(state.find_segment_at_offset(0, 0), Some(0));

        // Offset 49 is in init region of segment 0
        assert_eq!(state.find_segment_at_offset(0, 49), Some(0));

        // Offset 50 = media starts within segment 0
        assert_eq!(state.find_segment_at_offset(0, 50), Some(0));

        // Exact boundaries between segments
        assert_eq!(state.find_segment_at_offset(0, 149), Some(0)); // last byte of seg 0
        assert_eq!(state.find_segment_at_offset(0, 150), Some(1)); // first byte of seg 1
        assert_eq!(state.find_segment_at_offset(0, 249), Some(1)); // last byte of seg 1
        assert_eq!(state.find_segment_at_offset(0, 250), Some(2)); // first byte of seg 2
        assert_eq!(state.find_segment_at_offset(0, 349), Some(2)); // last byte of seg 2

        // Past end
        assert_eq!(state.find_segment_at_offset(0, 350), None);
        assert_eq!(state.find_segment_at_offset(0, 999), None);

        // Invalid variant
        assert_eq!(state.find_segment_at_offset(99, 50), None);
    }

    // ---- Test 7: from_parsed builder ----

    #[test]
    fn test_from_parsed_basic() {
        use crate::parsing::{
            CodecInfo, InitSegment, MediaPlaylist, MediaSegment, VariantId, VariantStream,
        };

        let variants = vec![VariantStream {
            id: VariantId(0),
            uri: "v0.m3u8".to_string(),
            bandwidth: Some(128_000),
            name: None,
            codec: Some(CodecInfo {
                codecs: Some("mp4a.40.2".to_string()),
                audio_codec: Some(AudioCodec::AacLc),
                container: Some(ContainerFormat::Fmp4),
            }),
        }];

        let media_url = Url::parse("https://cdn.example.com/audio/v0.m3u8").unwrap();
        let playlist = MediaPlaylist {
            segments: vec![
                MediaSegment {
                    sequence: 0,
                    variant_id: VariantId(0),
                    uri: "segment-0.m4s".to_string(),
                    duration: Duration::from_secs(4),
                    key: None,
                },
                MediaSegment {
                    sequence: 1,
                    variant_id: VariantId(0),
                    uri: "segment-1.m4s".to_string(),
                    duration: Duration::from_secs(4),
                    key: None,
                },
            ],
            target_duration: Some(Duration::from_secs(4)),
            init_segment: Some(InitSegment {
                uri: "init.mp4".to_string(),
                key: None,
            }),
            media_sequence: 0,
            end_list: true,
            current_key: None,
            detected_container: Some(ContainerFormat::Fmp4),
            allow_cache: true,
        };

        let media_playlists = vec![(media_url, playlist)];
        let state = PlaylistState::from_parsed(&variants, &media_playlists);

        // Verify variant count
        assert_eq!(state.num_variants(), 1);

        // Verify segment count
        assert_eq!(state.num_segments(0), Some(2));

        // Verify codec and container
        assert_eq!(state.variant_codec(0), Some(AudioCodec::AacLc));
        assert_eq!(state.variant_container(0), Some(ContainerFormat::Fmp4));

        // Verify segment URLs resolved correctly
        let seg0_url = state.segment_url(0, 0).unwrap();
        assert!(
            seg0_url.as_str().contains("segment-0.m4s"),
            "segment URL should contain segment-0.m4s, got: {seg0_url}"
        );

        let seg1_url = state.segment_url(0, 1).unwrap();
        assert!(
            seg1_url.as_str().contains("segment-1.m4s"),
            "segment URL should contain segment-1.m4s, got: {seg1_url}"
        );

        // Verify init URL resolved correctly
        let init = state.init_url(0).unwrap();
        assert!(
            init.as_str().contains("init.mp4"),
            "init URL should contain init.mp4, got: {init}"
        );

        // No size map yet
        assert!(!state.has_size_map(0));
    }
}
