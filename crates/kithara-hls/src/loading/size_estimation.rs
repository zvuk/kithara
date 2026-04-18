//! Segment size estimation strategies (avoids HEAD requests when possible).
//!
//! Tried in order:
//! 1. `#EXT-X-BYTERANGE` — exact sizes from the media playlist.
//! 2. Init segment bitrate — `avg_bitrate` from fMP4 `esds` descriptor.
//! 3. HEAD requests — network fallback.

use kithara_stream::dl::FetchCmd;
use re_mp4::{Mp4, StsdBoxContent};
use tracing::debug;

const BITS_PER_BYTE: f64 = 8.0;

use crate::{
    loading::SegmentLoader,
    parsing::MediaPlaylist,
    playlist::{PlaylistAccess, PlaylistState, VariantSizeMap},
};

/// Estimate segment sizes for all variants that don't have a size map yet.
///
/// Tries three strategies in order, per variant:
/// 1. Byte-range from playlist (`#EXT-X-BYTERANGE`)
/// 2. Average bitrate from init segment (fMP4 `esds`)
/// 3. HEAD requests (network fallback)
pub(crate) async fn estimate_size_maps(
    peer_handle: &kithara_stream::dl::PeerHandle,
    playlist_state: &PlaylistState,
    loader: &SegmentLoader,
    media_playlists: &[(url::Url, MediaPlaylist)],
    headers: Option<&kithara_net::Headers>,
) {
    let num_variants = playlist_state.num_variants();
    let mut buf = kithara_bufpool::byte_pool().get();

    for variant in 0..num_variants {
        if playlist_state.has_size_map(variant) {
            continue;
        }

        let num_segments = playlist_state.num_segments(variant).unwrap_or(0);
        if num_segments == 0 {
            continue;
        }

        // Strategy 1: EXT-X-BYTERANGE
        if let Some(size_map) = try_byte_range(media_playlists, variant, num_segments) {
            debug!(variant, "size_map: from EXT-X-BYTERANGE");
            playlist_state.set_size_map(variant, size_map);
            continue;
        }

        // Strategy 2: Init segment avg_bitrate
        if let Some(size_map) =
            try_init_bitrate(loader, media_playlists, variant, num_segments, &mut buf)
        {
            debug!(variant, "size_map: from init segment avg_bitrate");
            playlist_state.set_size_map(variant, size_map);
            continue;
        }

        // Strategy 3: HEAD requests (fallback)
        if let Some(size_map) =
            try_head_requests(peer_handle, playlist_state, variant, num_segments, headers).await
        {
            debug!(variant, "size_map: from HEAD requests");
            playlist_state.set_size_map(variant, size_map);
        }
    }
}

/// Strategy 1: exact sizes from `#EXT-X-BYTERANGE`.
fn try_byte_range(
    media_playlists: &[(url::Url, MediaPlaylist)],
    variant: usize,
    num_segments: usize,
) -> Option<VariantSizeMap> {
    let (_, playlist) = media_playlists.get(variant)?;

    // All segments must have byte_range_len for this strategy to work.
    let all_have_range = playlist
        .segments
        .iter()
        .take(num_segments)
        .all(|s| s.byte_range_len.is_some());

    if !all_have_range {
        return None;
    }

    let init_size = playlist.init_segment.as_ref().map_or(0, |_init| 0); // Init size unknown from byte range alone.

    let mut offsets = Vec::with_capacity(num_segments);
    let mut segment_sizes = Vec::with_capacity(num_segments);
    let mut cumulative = 0u64;

    for (i, seg) in playlist.segments.iter().take(num_segments).enumerate() {
        let media_len = seg.byte_range_len.unwrap_or(0);
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
    })
}

/// Strategy 2: estimate from init segment's average bitrate.
fn try_init_bitrate(
    loader: &SegmentLoader,
    media_playlists: &[(url::Url, MediaPlaylist)],
    variant: usize,
    num_segments: usize,
    buf: &mut Vec<u8>,
) -> Option<VariantSizeMap> {
    let init_meta = loader.get_init_segment_cached(variant)?;
    let init_len = init_meta.len;

    // Read init segment bytes from cache.
    loader.read_init_bytes(variant, buf)?;
    let avg_bitrate = extract_avg_bitrate(buf)?;

    let (_, playlist) = media_playlists.get(variant)?;

    let mut offsets = Vec::with_capacity(num_segments);
    let mut segment_sizes = Vec::with_capacity(num_segments);
    let mut cumulative = 0u64;

    for (i, seg) in playlist.segments.iter().take(num_segments).enumerate() {
        let duration_secs = seg.duration.as_secs_f64();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "bitrate × duration is always non-negative and fits u64"
        )]
        let media_len = (f64::from(avg_bitrate) / BITS_PER_BYTE * duration_secs) as u64;
        let total_seg = if i == 0 {
            init_len + media_len
        } else {
            media_len
        };
        offsets.push(cumulative);
        segment_sizes.push(total_seg);
        cumulative += total_seg;
    }

    debug!(
        variant,
        avg_bitrate,
        init_len,
        num_segments,
        estimated_total = cumulative,
        "init bitrate estimation"
    );

    Some(VariantSizeMap {
        segment_sizes,
        offsets,
        total: cumulative,
    })
}

/// Extract average bitrate (bits/sec) from fMP4 init segment.
///
/// Walks `moov/trak/mdia/minf/stbl/stsd` → `Mp4a` → `esds` →
/// `DecoderConfigDescriptor.avg_bitrate`.
fn extract_avg_bitrate(data: &[u8]) -> Option<u32> {
    let mp4 = Mp4::read_bytes(data).ok()?;
    for track in mp4.tracks().values() {
        let trak = track.trak(&mp4);
        let stsd = &trak.mdia.minf.stbl.stsd;
        if let StsdBoxContent::Mp4a(ref mp4a) = stsd.contents
            && let Some(ref esds) = mp4a.esds
        {
            let avg = esds.es_desc.dec_config.avg_bitrate;
            if avg > 0 {
                return Some(avg);
            }
        }
    }
    None
}

fn parse_content_length(resp: &kithara_stream::dl::FetchResponse) -> u64 {
    resp.headers
        .get("content-length")
        .or_else(|| resp.headers.get("Content-Length"))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Strategy 3: HEAD requests for Content-Length (network fallback).
async fn try_head_requests(
    peer_handle: &kithara_stream::dl::PeerHandle,
    playlist_state: &PlaylistState,
    variant: usize,
    num_segments: usize,
    headers: Option<&kithara_net::Headers>,
) -> Option<VariantSizeMap> {
    let init_url = playlist_state.init_url(variant);
    let mut cmds: Vec<FetchCmd> = Vec::new();
    let has_init = init_url.is_some();

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
    })
}
