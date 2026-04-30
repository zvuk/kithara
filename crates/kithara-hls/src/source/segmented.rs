use std::{ops::Range, sync::Arc};

use kithara_platform::{Mutex, time::Duration};
use kithara_stream::{SegmentDescriptor, SegmentedSource};

use crate::{
    playlist::{PlaylistAccess, PlaylistState},
    stream_index::StreamIndex,
};

/// Segment-aware view over the HLS source's shared state.
///
/// Lives behind `Arc` so it can be cloned into the decoder factory
/// (`DecoderConfig::segmented_source`) and queried by the segment
/// demuxer without holding any reference to `HlsSource` itself.
///
/// Reads against the *current layout variant* held by `StreamIndex` —
/// same byte space the decoder sees through `wait_range` / `read_at`.
pub(crate) struct HlsSegmentView {
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) segments: Arc<Mutex<StreamIndex>>,
}

impl HlsSegmentView {
    pub(crate) fn new(
        playlist_state: Arc<PlaylistState>,
        segments: Arc<Mutex<StreamIndex>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            playlist_state,
            segments,
        })
    }

    fn current_variant(&self) -> usize {
        let segs = self.segments.lock_sync();
        let layout = segs.layout_variant();
        let total = segs.num_variants();
        if layout < total { layout } else { 0 }
    }

    fn descriptor_for_segment(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> Option<SegmentDescriptor> {
        // Prefer the actually-committed byte range from `StreamIndex`:
        // after `reconcile_segment_size` (DRM decryption, post-HEAD
        // size correction) the playlist's estimated `segment_sizes`
        // may underreport, leaving the playlist-derived byte range
        // truncated mid-`mdat`. The committed range is always exact.
        let committed_range = self
            .segments
            .lock_sync()
            .item_range((variant, segment_index));
        let (start, next) = if let Some(range) = committed_range {
            (range.start, range.end)
        } else {
            let start = self
                .playlist_state
                .segment_byte_offset(variant, segment_index)?;
            let next = self
                .playlist_state
                .segment_byte_offset(variant, segment_index + 1)
                .or_else(|| self.playlist_state.total_variant_size(variant))?;
            (start, next)
        };
        if next <= start {
            return None;
        }
        let (decode_start, decode_end) = self
            .playlist_state
            .segment_decode_range(variant, segment_index)?;
        let duration = decode_end.saturating_sub(decode_start);
        let segment_index_u32 = u32::try_from(segment_index).ok()?;
        Some(SegmentDescriptor::new(
            start..next,
            decode_start,
            duration,
            segment_index_u32,
            variant,
        ))
    }
}

impl SegmentedSource for HlsSegmentView {
    fn init_segment_range(&self) -> Option<Range<u64>> {
        let variant = self.current_variant();
        let segments = self.segments.lock_sync();
        let vs = segments.variant_segments(variant)?;
        let (seg_idx, seg_data) = vs.iter().find(|(_, data)| data.init_len > 0)?;
        if seg_idx == 0 {
            let seg_range = segments.item_range((variant, 0))?;
            if seg_data.init_url.is_none() {
                drop(segments);
                return self
                    .playlist_state
                    .segment_byte_offset(variant, 0)
                    .and_then(|start| {
                        self.playlist_state
                            .segment_byte_offset(variant, 1)
                            .or_else(|| self.playlist_state.total_variant_size(variant))
                            .filter(|&end| end > start)
                            .map(|end| start..end)
                    });
            }
            return Some(seg_range);
        }
        if let Some(seg0_range) = segments.item_range((variant, 0)) {
            return Some(seg0_range);
        }
        drop(segments);
        let start = self.playlist_state.segment_byte_offset(variant, 0)?;
        let end = self
            .playlist_state
            .segment_byte_offset(variant, 1)
            .or_else(|| self.playlist_state.total_variant_size(variant))?;
        (end > start).then_some(start..end)
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        let variant = self.current_variant();
        let (segment_index, _, _) = self.playlist_state.find_seek_point_for_time(variant, t)?;
        // Already routed through `descriptor_for_segment` so the
        // committed-range preference applies here too.
        self.descriptor_for_segment(variant, segment_index)
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        let variant = self.current_variant();
        let segment_index = self
            .playlist_state
            .find_segment_at_offset(variant, byte_offset)?;
        let start = self
            .playlist_state
            .segment_byte_offset(variant, segment_index)?;
        let actual_index = if byte_offset > start {
            segment_index + 1
        } else {
            segment_index
        };
        self.descriptor_for_segment(variant, actual_index)
    }

    fn segment_count(&self) -> Option<u32> {
        let variant = self.current_variant();
        let count = self.playlist_state.num_segments(variant)?;
        u32::try_from(count).ok()
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::Mutex;
    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;
    use crate::{
        playlist::{SegmentState, VariantSizeMap, VariantState},
        stream_index::StreamIndex,
    };

    fn build_view(num_segments: usize, init_size: u64, media_sizes: &[u64]) -> Arc<HlsSegmentView> {
        let segments_state: Vec<SegmentState> = (0..num_segments)
            .map(|i| SegmentState {
                index: i,
                url: Url::parse(&format!("https://h.example/seg-{i}.m4s"))
                    .expect("valid segment URL"),
                duration: Duration::from_secs(4),
                key: None,
            })
            .collect();
        let variant = VariantState {
            id: 0,
            uri: Url::parse("https://h.example/v0.m3u8").expect("valid uri"),
            bandwidth: Some(128_000),
            codec: Some(kithara_stream::AudioCodec::AacLc),
            container: Some(kithara_stream::ContainerFormat::Fmp4),
            init_url: Some(Url::parse("https://h.example/init.mp4").expect("valid init")),
            segments: segments_state,
            size_map: None,
        };
        let playlist_state = Arc::new(PlaylistState::new(vec![variant]));

        let mut offsets = Vec::with_capacity(media_sizes.len());
        let mut segment_sizes = Vec::with_capacity(media_sizes.len());
        let mut cumulative = 0u64;
        for (i, &size) in media_sizes.iter().enumerate() {
            let total = if i == 0 { init_size + size } else { size };
            offsets.push(cumulative);
            segment_sizes.push(total);
            cumulative += total;
        }
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                segment_sizes,
                offsets,
                total: cumulative,
            },
        );

        let stream_index = StreamIndex::new(1, num_segments);
        let segments = Arc::new(Mutex::new(stream_index));
        HlsSegmentView::new(playlist_state, segments)
    }

    #[kithara::test]
    fn segment_at_time_maps_target_to_correct_descriptor() {
        // 4 segments, init=50, media=[100,100,100,100]
        // segment_sizes=[150,100,100,100], offsets=[0,150,250,350], total=450
        let view = build_view(4, 50, &[100, 100, 100, 100]);

        // Seek mid-segment-2: 8s..12s, byte range [250..350)
        let desc = view
            .segment_at_time(Duration::from_millis(8_500))
            .expect("segment");
        assert_eq!(desc.segment_index, 2);
        assert_eq!(desc.byte_range, 250..350);
        assert_eq!(desc.decode_time, Duration::from_secs(8));
        assert_eq!(desc.duration, Duration::from_secs(4));
        assert_eq!(desc.variant_index, 0);
    }

    #[kithara::test]
    fn segment_at_time_for_last_segment_uses_total_variant_size() {
        let view = build_view(4, 50, &[100, 100, 100, 100]);

        // Seek into segment 3: 12s..16s, byte range [350..450)
        let desc = view
            .segment_at_time(Duration::from_millis(13_000))
            .expect("last segment");
        assert_eq!(desc.segment_index, 3);
        assert_eq!(desc.byte_range, 350..450);
    }

    #[kithara::test]
    fn segment_at_time_clamps_to_tail_when_target_past_end() {
        let view = build_view(4, 50, &[100, 100, 100, 100]);

        // Past end of track — `find_seek_point_for_time` clamps to last segment.
        let desc = view
            .segment_at_time(Duration::from_secs(999))
            .expect("clamped");
        assert_eq!(desc.segment_index, 3);
    }

    #[kithara::test]
    fn segment_after_byte_advances_when_offset_is_mid_segment() {
        let view = build_view(4, 50, &[100, 100, 100, 100]);

        // 200 is mid-segment-1 ([150..250)) — sequential play wants segment 2.
        let desc = view.segment_after_byte(200).expect("segment");
        assert_eq!(desc.segment_index, 2);
        assert_eq!(desc.byte_range, 250..350);
    }

    #[kithara::test]
    fn segment_after_byte_returns_aligned_segment_when_at_boundary() {
        let view = build_view(4, 50, &[100, 100, 100, 100]);

        // 250 is exactly the start of segment 2 — return segment 2.
        let desc = view.segment_after_byte(250).expect("segment");
        assert_eq!(desc.segment_index, 2);
    }

    #[kithara::test]
    fn segment_count_reports_layout_variant_count() {
        let view = build_view(7, 50, &[100, 100, 100, 100, 100, 100, 100]);
        assert_eq!(view.segment_count(), Some(7));
    }
}
