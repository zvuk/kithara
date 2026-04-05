use kithara_events::SeekEpoch;

use crate::{
    ids::{SegmentId, SegmentIndex, VariantIndex},
    playlist::{PlaylistAccess, PlaylistState},
    stream_index::StreamIndex,
};

pub(crate) fn is_stale_epoch(fetch_epoch: SeekEpoch, current_epoch: SeekEpoch) -> bool {
    fetch_epoch != current_epoch
}

pub(crate) fn is_cross_codec_switch(
    playlist_state: &PlaylistState,
    from_variant: VariantIndex,
    to_variant: VariantIndex,
) -> bool {
    matches!(
        (
            playlist_state.variant_codec(from_variant),
            playlist_state.variant_codec(to_variant),
        ),
        (Some(from_codec), Some(to_codec)) if from_codec != to_codec
    )
}

pub(crate) fn first_missing_segment(
    state: &StreamIndex,
    variant: VariantIndex,
    start_segment: SegmentIndex,
    num_segments: usize,
) -> Option<SegmentIndex> {
    let start = start_segment.min(num_segments);
    (start..num_segments).find(|&segment_index| !state.is_segment_loaded(variant, segment_index))
}

pub(crate) fn classify_layout_transition(
    layout_variant: VariantIndex,
    variant: VariantIndex,
    segment_index: SegmentIndex,
) -> (bool, bool) {
    let is_variant_switch = layout_variant != variant;
    let is_midstream_switch = is_variant_switch && segment_index > 0;
    (is_variant_switch, is_midstream_switch)
}

pub(crate) fn should_request_init(is_variant_switch: bool, segment: SegmentId) -> bool {
    match segment {
        SegmentId::Init | SegmentId::Media(0) => true,
        SegmentId::Media(_) => is_variant_switch,
    }
}
