use std::sync::Arc;

use kithara_stream::dl::{FetchCmd, FetchResponse, PeerHandle};
use tracing::debug;
use url::Url;

use crate::{
    parsing::MediaPlaylist,
    playlist::{PlaylistAccess, PlaylistState, VariantSizeMap},
};

/// Owns the inputs required to compute [`VariantSizeMap`]s. Pure: the
/// estimator does not touch [`PlaylistState`]; the caller applies the
/// returned maps via [`PlaylistState::set_size_map`]. `media_playlists`
/// is moved in and returned by value alongside the maps so the caller
/// can keep using it after estimation.
///
/// Two strategies are tried in order, per variant:
/// 1. Byte-range from the media playlist (`#EXT-X-BYTERANGE`) — offline.
/// 2. HEAD requests against init/media URLs (network fallback).
///
/// The previous "init segment average-bitrate" strategy required the init
/// segment to already be cached. In the pull-driven architecture init
/// segments are fetched on demand by
/// [`HlsVariant::dispatch`](crate::variant::HlsVariant) *after* this
/// estimator runs, so the bitrate strategy was always a no-op and has
/// been removed.
pub(crate) struct SizeEstimator {
    peer: PeerHandle,
    playlist: Arc<PlaylistState>,
    media_playlists: Vec<MediaPlaylist>,
    headers: Option<kithara_net::Headers>,
}

/// Result of [`SizeEstimator::estimate`]: a `size_maps` vector indexed by
/// variant (always the same length as `media_playlists`) and the
/// originally-moved `media_playlists` returned by value. Variants that
/// already had a size map or that failed both estimation strategies land
/// as [`VariantSizeMap::is_empty`] entries; the caller skips those when
/// folding maps back into [`PlaylistState`].
pub(crate) struct Estimation {
    pub(crate) media_playlists: Vec<MediaPlaylist>,
    pub(crate) size_maps: Vec<VariantSizeMap>,
}

impl SizeEstimator {
    pub(crate) fn new(
        peer: PeerHandle,
        playlist: Arc<PlaylistState>,
        media_playlists: Vec<MediaPlaylist>,
        headers: Option<kithara_net::Headers>,
    ) -> Self {
        Self {
            peer,
            playlist,
            media_playlists,
            headers,
        }
    }

    /// Estimate every variant. The returned `size_maps` is indexed by
    /// variant: failed/skipped entries are [`VariantSizeMap::default`].
    pub(crate) async fn estimate(self) -> Estimation {
        let mut size_maps = Vec::with_capacity(self.playlist.num_variants());
        for variant in 0..self.playlist.num_variants() {
            size_maps.push(self.estimate_variant(variant).await);
        }
        Estimation {
            media_playlists: self.media_playlists,
            size_maps,
        }
    }

    async fn estimate_variant(&self, variant: usize) -> VariantSizeMap {
        if self.playlist.has_size_map(variant) {
            return VariantSizeMap::default();
        }
        let Some(num_segments) = self.playlist.num_segments(variant) else {
            return VariantSizeMap::default();
        };
        if num_segments == 0 {
            return VariantSizeMap::default();
        }
        if let Some(map) = self.try_byte_range(variant, num_segments) {
            debug!(variant, "size_map: from EXT-X-BYTERANGE");
            return map;
        }
        if let Some(map) = self.try_head_requests(variant, num_segments).await {
            debug!(variant, "size_map: from HEAD requests");
            return map;
        }
        VariantSizeMap::default()
    }

    /// Strategy 1: exact sizes from `#EXT-X-BYTERANGE` on every segment.
    fn try_byte_range(&self, variant: usize, num_segments: usize) -> Option<VariantSizeMap> {
        let playlist = self.media_playlists.get(variant)?;
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
        // EXT-X-BYTERANGE describes media ranges only — the init prefix
        // size is unknown here and HEAD fallback covers fMP4 variants.
        Some(VariantSizeMap {
            segment_sizes,
            offsets,
            total: cumulative,
            init_size: 0,
        })
    }

    /// Strategy 2: HEAD requests for `Content-Length` on init + every
    /// media segment URL. Folds `init_size` into `segment_sizes[0]` so
    /// the reader's variant-byte space starts at offset 0 with the init
    /// prefix already absorbed.
    async fn try_head_requests(
        &self,
        variant: usize,
        num_segments: usize,
    ) -> Option<VariantSizeMap> {
        let init_url = self.playlist.init_url(variant);
        let mut cmds: Vec<FetchCmd> = Vec::new();
        if let Some(ref url) = init_url {
            cmds.push(self.head_cmd(url.clone()));
        }
        for i in 0..num_segments {
            if let Some(url) = self.playlist.segment_url(variant, i) {
                cmds.push(self.head_cmd(url));
            }
        }
        if cmds.is_empty() {
            return None;
        }

        let results = self.peer.batch(cmds).await;
        let mut iter = results.iter();
        let init_size = init_url
            .as_ref()
            .and_then(|_| iter.next())
            .and_then(|r| r.as_ref().ok())
            .map_or(0, Self::content_length);

        let mut offsets = Vec::with_capacity(num_segments);
        let mut segment_sizes = Vec::with_capacity(num_segments);
        let mut cumulative = 0u64;
        for i in 0..num_segments {
            let media_len = iter
                .next()
                .and_then(|r| r.as_ref().ok())
                .map_or(0, Self::content_length);
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

    fn head_cmd(&self, url: Url) -> FetchCmd {
        FetchCmd::head(url).headers(self.headers.clone())
    }

    fn content_length(resp: &FetchResponse) -> u64 {
        resp.headers
            .get("content-length")
            .or_else(|| resp.headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    }
}
