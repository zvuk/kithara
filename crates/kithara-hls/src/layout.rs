use crate::{
    download_state::DownloadState,
    playlist::{PlaylistAccess, PlaylistState},
};

#[derive(Clone, Copy)]
pub(crate) struct LayoutAnchor {
    pub(crate) segment_index: usize,
    pub(crate) loaded_offset: u64,
    pub(crate) post_anchor_delta: i128,
}

pub(crate) fn expected_layout_offset(
    playlist_state: &PlaylistState,
    segments: &DownloadState,
    variant: usize,
    segment_index: usize,
    allow_infer: bool,
    had_midstream_switch: bool,
) -> Option<u64> {
    let metadata_offset = playlist_state.segment_byte_offset(variant, segment_index)?;
    let anchor = inferred_layout_anchor(
        playlist_state,
        segments,
        variant,
        allow_infer,
        had_midstream_switch,
    );
    Some(match anchor {
        Some(anchor) if segment_index == anchor.segment_index => anchor.loaded_offset,
        Some(anchor) if segment_index > anchor.segment_index => {
            let shifted = i128::from(metadata_offset) + anchor.post_anchor_delta;
            if shifted < 0 || shifted > i128::from(u64::MAX) {
                metadata_offset
            } else {
                u64::try_from(shifted).unwrap_or(metadata_offset)
            }
        }
        _ => metadata_offset,
    })
}

pub(crate) fn find_segment_at_layout_offset(
    playlist_state: &PlaylistState,
    segments: &DownloadState,
    variant: usize,
    offset: u64,
    allow_infer: bool,
    had_midstream_switch: bool,
) -> Option<usize> {
    let anchor = inferred_layout_anchor(
        playlist_state,
        segments,
        variant,
        allow_infer,
        had_midstream_switch,
    );
    let Some(anchor) = anchor else {
        return playlist_state.find_segment_at_offset(variant, offset);
    };

    let next_metadata = playlist_state
        .segment_byte_offset(variant, anchor.segment_index.saturating_add(1))
        .or_else(|| playlist_state.total_variant_size(variant))?;
    let anchor_end = i128::from(next_metadata) + anchor.post_anchor_delta;
    if i128::from(offset) >= i128::from(anchor.loaded_offset) && i128::from(offset) < anchor_end {
        return Some(anchor.segment_index);
    }

    let translated = i128::from(offset) - anchor.post_anchor_delta;
    if translated < 0 || translated > i128::from(u64::MAX) {
        return playlist_state.find_segment_at_offset(variant, offset);
    }

    u64::try_from(translated).ok().map_or_else(
        || playlist_state.find_segment_at_offset(variant, offset),
        |translated| playlist_state.find_segment_at_offset(variant, translated),
    )
}

pub(crate) fn inferred_layout_anchor(
    playlist_state: &PlaylistState,
    segments: &DownloadState,
    variant: usize,
    allow_infer: bool,
    had_midstream_switch: bool,
) -> Option<LayoutAnchor> {
    if !allow_infer {
        return None;
    }

    let first = segments.first_segment_of_variant(variant)?;
    let first_metadata = playlist_state.segment_byte_offset(variant, first.segment_index)?;
    let first_delta = i128::from(first.byte_offset) - i128::from(first_metadata);

    let should_infer = if had_midstream_switch {
        true
    } else {
        let last = segments.last_of_variant(variant)?;
        let last_metadata = playlist_state.segment_byte_offset(variant, last.segment_index)?;
        let last_delta = i128::from(last.byte_offset) - i128::from(last_metadata);
        let inferred_from_init = first.init_len > 0 && first.segment_index > 0 && first_delta != 0;
        let inferred_from_consistent_delta = (first.segment_index != last.segment_index
            || first.byte_offset != last.byte_offset)
            && first_delta == last_delta;
        inferred_from_init || inferred_from_consistent_delta
    };

    if !should_infer {
        return None;
    }

    let post_anchor_delta = playlist_state
        .segment_byte_offset(variant, first.segment_index.saturating_add(1))
        .map_or(first_delta, |next_metadata| {
            i128::from(first.byte_offset.saturating_add(first.total_len()))
                - i128::from(next_metadata)
        });

    Some(LayoutAnchor {
        segment_index: first.segment_index,
        loaded_offset: first.byte_offset,
        post_anchor_delta,
    })
}
