use kithara_platform::{RwLock, time::Duration};
use kithara_stream::{AudioCodec, ContainerFormat};
use url::Url;

use crate::ids::{SegmentIndex, VariantIndex};

/// Per-segment parsed data (from media playlist).
#[derive(Debug, Clone)]
pub struct SegmentState {
    /// Duration of the segment.
    pub duration: Duration,
    /// Segment index within the variant's media playlist.
    pub index: SegmentIndex,
    /// Absolute URL of the segment.
    pub url: Url,
}

/// Per-variant size information (from HEAD requests or download).
///
/// Uses a 0-based offset model: the first segment starts at offset 0 and
/// includes init data, so `segment_sizes[0]` already folds the init
/// size into the segment-0 total.
#[derive(Debug, Clone, Default)]
pub struct VariantSizeMap {
    /// Cumulative byte offsets. `offsets[0]` = 0, `offsets[i]` = sum of `segment_sizes[0..i]`.
    pub offsets: Vec<u64>,
    /// Per-segment total sizes in bytes. `segment_sizes[0]` includes the
    /// init segment size for fMP4 variants.
    pub segment_sizes: Vec<u64>,
    /// Total size of the variant (init + all segments).
    pub total: u64,
    /// Standalone init segment size in bytes — the first `init_size` bytes
    /// of segment 0 in variant-byte space resolve to the init resource.
    /// Zero for raw TS/AAC variants without `#EXT-X-MAP`.
    pub init_size: u64,
}

impl VariantSizeMap {
    /// `true` when this map carries no usable data — used by callers to
    /// skip applying a placeholder map produced by a failed estimation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0 && self.segment_sizes.is_empty()
    }
}

/// Per-variant parsed data.
#[derive(Debug)]
pub struct VariantState {
    /// Audio codec (parsed from CODECS attribute).
    pub codec: Option<AudioCodec>,
    /// Container format (detected from segment URIs).
    pub container: Option<ContainerFormat>,
    /// Absolute URL of the init segment (fMP4 only).
    pub init_url: Option<Url>,
    /// Size map (populated after HEAD requests or first download).
    pub size_map: Option<VariantSizeMap>,
    /// Parsed segments from the media playlist.
    pub segments: Vec<SegmentState>,
}

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
    #[must_use]
    pub fn from_parsed(
        variants: &[crate::parsing::VariantStream],
        media_playlists: &[crate::parsing::MediaPlaylist],
    ) -> Self {
        let variant_states: Vec<VariantState> = variants
            .iter()
            .zip(media_playlists.iter())
            .map(|(variant, playlist)| {
                let media_url = &playlist.url;
                let codec = variant.codec.as_ref().and_then(|ci| ci.audio_codec);
                let container = variant
                    .codec
                    .as_ref()
                    .and_then(|ci| ci.container)
                    .or(playlist.detected_container);

                let init_url = playlist
                    .init_segment
                    .as_ref()
                    .and_then(|init| media_url.join(&init.uri).ok());

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
                        }
                    })
                    .collect();

                VariantState {
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

    /// Number of segments in a variant's media playlist.
    #[must_use]
    pub fn num_segments(&self, variant: VariantIndex) -> Option<usize> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        Some(state.segments.len())
    }

    /// Number of variants in the master playlist.
    #[must_use]
    pub fn num_variants(&self) -> usize {
        self.variants.len()
    }

    /// Reconcile a segment's actual size after DRM decryption.
    ///
    /// Updates the segment's size and recalculates all subsequent offsets.
    /// This handles the case where decrypted size differs from encrypted size.
    pub fn reconcile_segment_size(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        actual_total: u64,
    ) {
        let Some(lock) = self.variants.get(variant) else {
            return;
        };
        let mut state = lock.lock_sync_write();
        let Some(size_map) = state.size_map.as_mut() else {
            return;
        };
        if segment_index >= size_map.segment_sizes.len() {
            return;
        }

        size_map.segment_sizes[segment_index] = actual_total;

        for i in (segment_index + 1)..size_map.offsets.len() {
            size_map.offsets[i] = size_map.offsets[i - 1] + size_map.segment_sizes[i - 1];
        }

        let last_idx = size_map.segment_sizes.len() - 1;
        size_map.total = size_map.offsets[last_idx] + size_map.segment_sizes[last_idx];
        drop(state);
    }

    /// Clone of `segment_sizes` from the `size_map` (for `StreamIndex` sync).
    #[must_use]
    pub fn segment_sizes(&self, variant: VariantIndex) -> Option<Vec<u64>> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        state.size_map.as_ref().map(|sm| sm.segment_sizes.clone())
    }

    /// Set the size map for a variant (after HEAD requests or first download).
    pub fn set_size_map(&self, variant: VariantIndex, size_map: VariantSizeMap) {
        if let Some(lock) = self.variants.get(variant) {
            let mut state = lock.lock_sync_write();
            state.size_map = Some(size_map);
        }
    }

    /// Total media duration across parsed variants.
    ///
    /// Variants are expected to describe the same timeline. We keep the
    /// maximum duration to tolerate incomplete alternate renditions.
    #[must_use]
    pub fn track_duration(&self) -> Option<Duration> {
        self.variants
            .iter()
            .map(|lock| {
                let state = lock.lock_sync_read();
                state.segments.iter().fold(Duration::ZERO, |acc, segment| {
                    acc.saturating_add(segment.duration)
                })
            })
            .max()
    }
}

/// Crate-internal namespace bundle for cross-module read access to
/// parsed playlist data. Not used polymorphically — the trait exists
/// solely to group the visibility of N methods under a single
/// `pub(crate)` marker rather than annotating each one individually.
pub(crate) trait PlaylistAccess: Send + Sync {
    fn find_seek_point_for_time(
        &self,
        variant: VariantIndex,
        target: Duration,
    ) -> Option<(SegmentIndex, Duration, Duration)>;

    fn has_size_map(&self, variant: VariantIndex) -> bool;
    fn init_url(&self, variant: VariantIndex) -> Option<Url>;
    fn init_size(&self, variant: VariantIndex) -> u64;
    fn segment_byte_offset(&self, variant: VariantIndex, index: SegmentIndex) -> Option<u64>;
    fn segment_decode_range(
        &self,
        variant: VariantIndex,
        index: SegmentIndex,
    ) -> Option<(Duration, Duration)>;
    fn segment_url(&self, variant: VariantIndex, index: SegmentIndex) -> Option<Url>;
    fn total_variant_size(&self, variant: VariantIndex) -> Option<u64>;
}

impl PlaylistAccess for PlaylistState {
    fn find_seek_point_for_time(
        &self,
        variant: VariantIndex,
        target: Duration,
    ) -> Option<(SegmentIndex, Duration, Duration)> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        if state.segments.is_empty() {
            return None;
        }

        let result = {
            let mut elapsed = Duration::ZERO;
            let mut found = None;
            for segment in &state.segments {
                let segment_start = elapsed;
                let segment_end = segment_start.saturating_add(segment.duration);
                if target < segment_end {
                    found = Some((segment.index, segment_start, segment_end));
                    break;
                }
                elapsed = segment_end;
            }
            found.or_else(|| {
                state.segments.last().map(|tail| {
                    let tail_start = elapsed.saturating_sub(tail.duration);
                    (tail.index, tail_start, elapsed)
                })
            })
        };
        drop(state);
        result
    }

    fn has_size_map(&self, variant: VariantIndex) -> bool {
        let Some(lock) = self.variants.get(variant) else {
            return false;
        };
        let state = lock.lock_sync_read();
        state.size_map.is_some()
    }

    fn init_url(&self, variant: VariantIndex) -> Option<Url> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        state.init_url.clone()
    }

    fn init_size(&self, variant: VariantIndex) -> u64 {
        let Some(lock) = self.variants.get(variant) else {
            return 0;
        };
        let state = lock.lock_sync_read();
        state.size_map.as_ref().map_or(0, |sm| sm.init_size)
    }

    fn segment_byte_offset(&self, variant: VariantIndex, index: SegmentIndex) -> Option<u64> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        state.size_map.as_ref()?.offsets.get(index).copied()
    }

    fn segment_decode_range(
        &self,
        variant: VariantIndex,
        index: SegmentIndex,
    ) -> Option<(Duration, Duration)> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        if index >= state.segments.len() {
            return None;
        }
        let result = state
            .segments
            .iter()
            .scan(Duration::ZERO, |elapsed, segment| {
                let start = *elapsed;
                let end = start.saturating_add(segment.duration);
                *elapsed = end;
                Some((start, end))
            })
            .nth(index);
        drop(state);
        result
    }

    fn segment_url(&self, variant: VariantIndex, index: SegmentIndex) -> Option<Url> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        state.segments.get(index).map(|s| s.url.clone())
    }

    fn total_variant_size(&self, variant: VariantIndex) -> Option<u64> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        state.size_map.as_ref().map(|sm| sm.total)
    }
}

#[cfg(test)]
impl PlaylistState {
    /// Find which segment contains the given byte offset (binary search on offsets).
    fn find_segment_at_offset(&self, variant: VariantIndex, offset: u64) -> Option<SegmentIndex> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        let result = state.size_map.as_ref().and_then(|size_map| {
            if size_map.offsets.is_empty() || offset >= size_map.total {
                return None;
            }
            let pos = size_map.offsets.partition_point(|&o| o <= offset);
            if pos == 0 { None } else { Some(pos - 1) }
        });
        drop(state);
        result
    }

    /// Audio codec for a variant.
    fn variant_codec(&self, variant: VariantIndex) -> Option<AudioCodec> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        state.codec
    }

    /// Container format for a variant.
    fn variant_container(&self, variant: VariantIndex) -> Option<ContainerFormat> {
        let lock = self.variants.get(variant)?;
        let state = lock.lock_sync_read();
        state.container
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;
    use crate::parsing::{
        CodecInfo, InitSegment, MediaPlaylist, MediaSegment, VariantId, VariantStream,
    };

    fn base_url() -> Url {
        Url::parse("https://cdn.example.com/audio/").expect("valid base URL")
    }

    fn test_url(raw: &str) -> Url {
        Url::parse(raw).expect("valid test URL")
    }

    fn make_segment(index: usize) -> SegmentState {
        SegmentState {
            index,
            url: base_url()
                .join(&format!("segment-{index}.m4s"))
                .expect("valid segment URL"),
            duration: Duration::from_secs(4),
        }
    }

    fn make_variant(_id: usize, num_segments: usize) -> VariantState {
        let segments: Vec<SegmentState> = (0..num_segments).map(make_segment).collect();
        VariantState {
            segments,
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            init_url: Some(base_url().join("init.mp4").expect("valid init URL")),
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
            segment_sizes,
            offsets,
            total: cumulative,
            init_size,
        }
    }

    fn seek_point_for_time(
        state: &PlaylistState,
        variant: usize,
        target_ms: u64,
    ) -> Option<(usize, u64)> {
        let (segment_index, _segment_start, _segment_end) =
            state.find_seek_point_for_time(variant, Duration::from_millis(target_ms))?;
        let byte_offset = state.segment_byte_offset(variant, segment_index)?;
        Some((segment_index, byte_offset))
    }

    #[kithara::test]
    fn test_playlist_state_basic_access() {
        let state = PlaylistState::new(vec![make_variant(0, 5), make_variant(1, 3)]);

        assert_eq!(state.num_variants(), 2);
        assert_eq!(state.num_segments(0), Some(5));
        assert_eq!(state.num_segments(1), Some(3));

        assert_eq!(state.num_segments(2), None);
        assert_eq!(state.num_segments(99), None);
    }

    #[kithara::test]
    fn test_playlist_state_variant_info() {
        let mut v = make_variant(0, 2);
        v.codec = Some(AudioCodec::Flac);
        v.container = Some(ContainerFormat::Fmp4);
        let state = PlaylistState::new(vec![v]);

        assert_eq!(state.variant_codec(0), Some(AudioCodec::Flac));
        assert_eq!(state.variant_container(0), Some(ContainerFormat::Fmp4));

        let url = state.segment_url(0, 0).unwrap();
        assert!(url.as_str().contains("segment-0.m4s"));
        let url1 = state.segment_url(0, 1).unwrap();
        assert!(url1.as_str().contains("segment-1.m4s"));
        assert_eq!(state.segment_url(0, 2), None);

        let init = state.init_url(0).unwrap();
        assert!(init.as_str().contains("init.mp4"));

        assert_eq!(state.variant_codec(5), None);
        assert_eq!(state.variant_container(5), None);
        assert_eq!(state.segment_url(5, 0), None);
        assert_eq!(state.init_url(5), None);
    }

    #[kithara::test]
    fn test_size_map_not_set() {
        let state = PlaylistState::new(vec![make_variant(0, 3)]);

        assert!(!state.has_size_map(0));
        assert_eq!(state.total_variant_size(0), None);
        assert_eq!(state.segment_byte_offset(0, 0), None);
        assert_eq!(state.find_segment_at_offset(0, 0), None);
    }

    #[kithara::test]
    fn test_size_map_set_and_query() {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);

        let sm = make_size_map(100, &[200, 300, 400, 500]);
        state.set_size_map(0, sm);

        assert!(state.has_size_map(0));
        assert_eq!(state.total_variant_size(0), Some(1500));
    }

    #[kithara::test(wasm)]
    #[case::first(0, Some(0))]
    #[case::second(1, Some(300))]
    #[case::third(2, Some(600))]
    #[case::fourth(3, Some(1000))]
    #[case::oob(4, None)]
    fn test_size_map_segment_offsets(#[case] segment: usize, #[case] expected: Option<u64>) {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);
        state.set_size_map(0, make_size_map(100, &[200, 300, 400, 500]));
        assert_eq!(state.segment_byte_offset(0, segment), expected);
    }

    #[kithara::test(wasm)]
    #[case::init_start(0, Some(0))]
    #[case::init_inside(99, Some(0))]
    #[case::media_start(100, Some(0))]
    #[case::first_end(299, Some(0))]
    #[case::second_start(300, Some(1))]
    #[case::second_end(599, Some(1))]
    #[case::third_start(600, Some(2))]
    #[case::fourth_start(1000, Some(3))]
    #[case::last_byte(1499, Some(3))]
    fn test_size_map_find_segment_at_offset(#[case] offset: u64, #[case] expected: Option<usize>) {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);
        state.set_size_map(0, make_size_map(100, &[200, 300, 400, 500]));
        assert_eq!(state.find_segment_at_offset(0, offset), expected);
    }

    #[kithara::test]
    fn test_reconcile_segment_size() {
        let state = PlaylistState::new(vec![make_variant(0, 3)]);

        let sm = make_size_map(100, &[200, 300, 400]);
        state.set_size_map(0, sm);

        assert_eq!(state.total_variant_size(0), Some(1000));
        assert_eq!(state.segment_byte_offset(0, 1), Some(300));
        assert_eq!(state.segment_byte_offset(0, 2), Some(600));

        state.reconcile_segment_size(0, 1, 250);

        assert_eq!(state.segment_byte_offset(0, 0), Some(0));
        assert_eq!(state.segment_byte_offset(0, 1), Some(300));
        assert_eq!(state.segment_byte_offset(0, 2), Some(550));
        assert_eq!(state.total_variant_size(0), Some(950));

        assert_eq!(state.find_segment_at_offset(0, 549), Some(1));
        assert_eq!(state.find_segment_at_offset(0, 550), Some(2));
    }

    #[kithara::test(wasm)]
    #[case::first_segment_start(0, 0, Some(0))]
    #[case::init_region(0, 49, Some(0))]
    #[case::first_media_start(0, 50, Some(0))]
    #[case::first_segment_end(0, 149, Some(0))]
    #[case::second_segment_start(0, 150, Some(1))]
    #[case::second_segment_end(0, 249, Some(1))]
    #[case::third_segment_start(0, 250, Some(2))]
    #[case::last_byte(0, 349, Some(2))]
    #[case::past_end(0, 350, None)]
    #[case::far_past_end(0, 999, None)]
    #[case::invalid_variant(99, 50, None)]
    fn test_find_segment_at_offset_edge_cases(
        #[case] variant: usize,
        #[case] offset: u64,
        #[case] expected: Option<usize>,
    ) {
        let state = PlaylistState::new(vec![make_variant(0, 3)]);

        let sm = make_size_map(50, &[100, 100, 100]);
        state.set_size_map(0, sm);
        assert_eq!(state.find_segment_at_offset(variant, offset), expected);
    }

    #[kithara::test(wasm)]
    #[case::segment_0_start(0, (0, 0))]
    #[case::segment_0_last_ms(3999, (0, 0))]
    #[case::segment_1_start(4000, (1, 150))]
    #[case::segment_1_last_ms(7999, (1, 150))]
    #[case::segment_2_start(8000, (2, 250))]
    #[case::segment_3_start(12000, (3, 350))]
    #[case::past_end_clamps_to_last(999_999, (3, 350))]
    fn test_seek_point_for_time_deterministic(
        #[case] target_ms: u64,
        #[case] expected: (usize, u64),
    ) {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);
        state.set_size_map(0, make_size_map(50, &[100, 100, 100, 100]));
        let seek_point = seek_point_for_time(&state, 0, target_ms).expect("seek point");
        assert_eq!(seek_point, expected);
    }

    #[kithara::test]
    fn test_seek_point_for_time_boundary_at_track_end() {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);
        state.set_size_map(0, make_size_map(50, &[100, 100, 100, 100]));

        let seek_point = seek_point_for_time(&state, 0, 16_000).expect("seek point");
        assert_eq!(seek_point, (3, 350));
    }

    #[kithara::test]
    fn test_find_seek_point_for_time_returns_segment_bounds() {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);
        let seek_point = state
            .find_seek_point_for_time(0, Duration::from_millis(8_500))
            .expect("seek point");

        assert_eq!(seek_point.0, 2);
        assert_eq!(seek_point.1, Duration::from_secs(8));
        assert_eq!(seek_point.2, Duration::from_secs(12));
    }

    #[kithara::test]
    fn test_track_duration_uses_longest_variant() {
        let state = PlaylistState::new(vec![make_variant(0, 4), make_variant(1, 3)]);
        assert_eq!(state.track_duration(), Some(Duration::from_secs(16)));
    }

    #[kithara::test(wasm)]
    #[case::first(0, Some((Duration::from_secs(0), Duration::from_secs(4))))]
    #[case::second(1, Some((Duration::from_secs(4), Duration::from_secs(8))))]
    #[case::third(2, Some((Duration::from_secs(8), Duration::from_secs(12))))]
    #[case::last(3, Some((Duration::from_secs(12), Duration::from_secs(16))))]
    #[case::out_of_range(4, None)]
    fn test_segment_decode_range(
        #[case] index: usize,
        #[case] expected: Option<(Duration, Duration)>,
    ) {
        let state = PlaylistState::new(vec![make_variant(0, 4)]);
        assert_eq!(state.segment_decode_range(0, index), expected);
    }

    #[kithara::test]
    fn test_from_parsed_basic() {
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

        let playlist = MediaPlaylist {
            url: test_url("https://cdn.example.com/audio/v0.m3u8"),
            segments: vec![
                MediaSegment {
                    sequence: 0,
                    uri: "segment-0.m4s".to_string(),
                    duration: Duration::from_secs(4),
                    key: None,
                    byte_range_len: None,
                },
                MediaSegment {
                    sequence: 1,
                    uri: "segment-1.m4s".to_string(),
                    duration: Duration::from_secs(4),
                    key: None,
                    byte_range_len: None,
                },
            ],
            init_segment: Some(InitSegment {
                uri: "init.mp4".to_string(),
                key: None,
            }),
            detected_container: Some(ContainerFormat::Fmp4),
        };

        let media_playlists = vec![playlist];
        let state = PlaylistState::from_parsed(&variants, &media_playlists);

        assert_eq!(state.num_variants(), 1);

        assert_eq!(state.num_segments(0), Some(2));

        assert_eq!(state.variant_codec(0), Some(AudioCodec::AacLc));
        assert_eq!(state.variant_container(0), Some(ContainerFormat::Fmp4));

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

        let init = state.init_url(0).unwrap();
        assert!(
            init.as_str().contains("init.mp4"),
            "init URL should contain init.mp4, got: {init}"
        );

        assert!(!state.has_size_map(0));
    }
}
