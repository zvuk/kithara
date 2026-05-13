use kithara_stream::dl::FetchCmd;
use tracing::debug;

use crate::{
    parsing::MediaPlaylist,
    playlist::{PlaylistAccess, PlaylistState, VariantSizeMap},
};

/// Estimate segment sizes for all variants that don't have a size map yet.
///
/// Tries two strategies in order, per variant:
/// 1. Byte-range from playlist (`#EXT-X-BYTERANGE`) — offline.
/// 2. HEAD requests (network fallback).
///
/// The previous "init segment average-bitrate" estimation strategy
/// required the init segment to already be cached. In the pull-driven
/// architecture init segments are fetched on demand by
/// [`HlsVariant::dispatch`](crate::variant::HlsVariant), after this
/// function runs — so the bitrate strategy was always a no-op here and
/// has been removed.
pub(crate) async fn estimate_size_maps(
    peer_handle: &kithara_stream::dl::PeerHandle,
    playlist_state: &PlaylistState,
    media_playlists: &[(url::Url, MediaPlaylist)],
    headers: Option<&kithara_net::Headers>,
) {
    for variant in 0..playlist_state.num_variants() {
        estimate_variant_size_map(
            peer_handle,
            playlist_state,
            media_playlists,
            headers,
            variant,
        )
        .await;
    }
}

async fn estimate_variant_size_map(
    peer_handle: &kithara_stream::dl::PeerHandle,
    playlist_state: &PlaylistState,
    media_playlists: &[(url::Url, MediaPlaylist)],
    headers: Option<&kithara_net::Headers>,
    variant: usize,
) {
    if playlist_state.has_size_map(variant) {
        return;
    }
    let Some(num_segments) = playlist_state.num_segments(variant) else {
        return;
    };
    if num_segments == 0 {
        return;
    }

    if let Some(size_map) = try_byte_range(media_playlists, variant, num_segments) {
        debug!(variant, "size_map: from EXT-X-BYTERANGE");
        playlist_state.set_size_map(variant, size_map);
        return;
    }
    if let Some(size_map) =
        try_head_requests(peer_handle, playlist_state, variant, num_segments, headers).await
    {
        debug!(variant, "size_map: from HEAD requests");
        playlist_state.set_size_map(variant, size_map);
    }
}

/// Strategy 1: exact sizes from `#EXT-X-BYTERANGE`.
fn try_byte_range(
    media_playlists: &[(url::Url, MediaPlaylist)],
    variant: usize,
    num_segments: usize,
) -> Option<VariantSizeMap> {
    let (_, playlist) = media_playlists.get(variant)?;

    let all_have_range = playlist
        .segments
        .iter()
        .take(num_segments)
        .all(|s| s.byte_range_len.is_some());
    if !all_have_range {
        return None;
    }

    let mut offsets = Vec::with_capacity(num_segments);
    let mut segment_sizes = Vec::with_capacity(num_segments);
    let mut cumulative = 0u64;
    for seg in playlist.segments.iter().take(num_segments) {
        let media_len = seg.byte_range_len.unwrap_or(0);
        offsets.push(cumulative);
        segment_sizes.push(media_len);
        cumulative += media_len;
    }

    // EXT-X-BYTERANGE strategy operates purely on media segment ranges,
    // so the init prefix size is unknown here — leave it at zero.
    Some(VariantSizeMap {
        segment_sizes,
        offsets,
        total: cumulative,
        init_size: 0,
    })
}

fn parse_content_length(resp: &kithara_stream::dl::FetchResponse) -> u64 {
    resp.headers
        .get("content-length")
        .or_else(|| resp.headers.get("Content-Length"))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Strategy 2: HEAD requests for `Content-Length` (network fallback).
async fn try_head_requests(
    peer_handle: &kithara_stream::dl::PeerHandle,
    playlist_state: &PlaylistState,
    variant: usize,
    num_segments: usize,
    headers: Option<&kithara_net::Headers>,
) -> Option<VariantSizeMap> {
    let init_url = playlist_state.init_url(variant);
    let has_init = init_url.is_some();
    let mut cmds: Vec<FetchCmd> = Vec::new();
    if let Some(ref url) = init_url {
        cmds.push(FetchCmd::head(url.clone()).headers(headers.cloned()));
    }
    for i in 0..num_segments {
        if let Some(url) = playlist_state.segment_url(variant, i) {
            cmds.push(FetchCmd::head(url).headers(headers.cloned()));
        }
    }
    if cmds.is_empty() {
        return None;
    }

    let results = peer_handle.batch(cmds).await;

    let mut idx = 0;
    let init_size = if has_init {
        let s = results
            .get(idx)
            .and_then(|r| r.as_ref().ok())
            .map_or(0, parse_content_length);
        idx += 1;
        s
    } else {
        0
    };

    let mut offsets = Vec::with_capacity(num_segments);
    let mut segment_sizes = Vec::with_capacity(num_segments);
    let mut cumulative = 0u64;
    for i in 0..num_segments {
        let media_len = results
            .get(idx)
            .and_then(|r| r.as_ref().ok())
            .map_or(0, parse_content_length);
        idx += 1;
        let total_seg = if i == 0 {
            init_size + media_len
        } else {
            media_len
        };
        offsets.push(cumulative);
        segment_sizes.push(total_seg);
        cumulative += total_seg;
    }

    Some(VariantSizeMap {
        segment_sizes,
        offsets,
        total: cumulative,
        init_size,
    })
}
