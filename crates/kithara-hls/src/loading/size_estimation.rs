use std::sync::Arc;

use kithara_assets::AssetScope;
use kithara_drm::DecryptContext;
use kithara_net::RangeSpec;
use kithara_stream::dl::{FetchCmd, FetchResponse, PeerHandle};
use tracing::debug;
use url::Url;

use crate::{
    config::SizeProbeMethod,
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
/// 2. Cache-first probes against init/media URLs: sizes of resources
///    already committed in the asset store come from `final_len`;
///    only cache misses go to the network (HEAD / ranged GET).
///
/// The previous "init segment average-bitrate" strategy required the init
/// segment to already be cached. In the pull-driven architecture init
/// segments are fetched on demand by
/// [`HlsVariant::dispatch`](crate::variant::HlsVariant) *after* this
/// estimator runs, so the bitrate strategy was always a no-op and has
/// been removed.
pub(crate) struct SizeEstimator {
    playlist: Arc<PlaylistState>,
    headers: Option<kithara_net::Headers>,
    peer: PeerHandle,
    /// Committed-resource lookup: same store the dispatch path consults
    /// before fetching, so estimation and fetch share one source of truth.
    scope: AssetScope<DecryptContext>,
    /// Whether probes are real `HEAD`s or single-byte ranged `GET`s.
    probe_method: SizeProbeMethod,
    media_playlists: Vec<MediaPlaylist>,
    /// Max probe requests in flight at once. `0` is normalised to
    /// `1`. Caps concurrency against upstreams that throttle or drop
    /// TCP on burst (e.g. zvuk stage `/drm/`).
    concurrency: usize,
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
        scope: AssetScope<DecryptContext>,
        playlist: Arc<PlaylistState>,
        media_playlists: Vec<MediaPlaylist>,
        headers: Option<kithara_net::Headers>,
        concurrency: usize,
        probe_method: SizeProbeMethod,
    ) -> Self {
        Self {
            playlist,
            headers,
            peer,
            scope,
            media_playlists,
            probe_method,
            concurrency: concurrency.max(1),
        }
    }

    fn content_length(resp: &FetchResponse) -> u64 {
        if let Some(total) = resp
            .headers
            .get("content-range")
            .or_else(|| resp.headers.get("Content-Range"))
            .and_then(|h| h.split('/').nth(1))
            .filter(|s| *s != "*")
            .and_then(|s| s.parse::<u64>().ok())
        {
            return total;
        }
        resp.headers
            .get("content-length")
            .or_else(|| resp.headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    }

    /// Estimate every variant. The returned `size_maps` is indexed by
    /// variant: failed/skipped entries are [`VariantSizeMap::default`].
    pub(crate) async fn estimate(self) -> Estimation {
        let mut size_maps = Vec::with_capacity(self.playlist.num_variants());
        for variant in 0..self.playlist.num_variants() {
            size_maps.push(self.estimate_variant(variant).await);
        }
        Estimation {
            size_maps,
            media_playlists: self.media_playlists,
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
        if let Some(map) = self.try_probe_sizes(variant, num_segments).await {
            debug!(variant, "size_map: from cache-first probes");
            return map;
        }
        VariantSizeMap::default()
    }

    /// Build the probe command for `url`. Strategy is driven by
    /// [`Self::probe_method`].
    fn probe_cmd(&self, url: Url) -> FetchCmd {
        match self.probe_method {
            SizeProbeMethod::Head => FetchCmd::head(url)
                .maybe_headers(self.headers.clone())
                .build(),
            SizeProbeMethod::RangeGet => FetchCmd::get(url)
                .range(RangeSpec::new(0, Some(0)))
                .maybe_headers(self.headers.clone())
                .build(),
        }
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
        Some(VariantSizeMap {
            segment_sizes,
            offsets,
            total: cumulative,
            init_size: 0,
        })
    }

    /// Strategy 2: cache-first sizes for init + every media segment URL.
    /// Resources committed in the asset store contribute their
    /// `final_len`; only cache misses are probed over the network. Folds
    /// `init_size` into `segment_sizes[0]` so the reader's variant-byte
    /// space starts at offset 0 with the init prefix already absorbed.
    async fn try_probe_sizes(&self, variant: usize, num_segments: usize) -> Option<VariantSizeMap> {
        let init_url = self.playlist.init_url(variant);
        debug!(
            variant,
            init_url = ?init_url.as_ref().map(Url::as_str),
            num_segments,
            "size_estimation: try_probe_sizes start"
        );
        let mut urls: Vec<Url> = Vec::with_capacity(num_segments + 1);
        if let Some(ref url) = init_url {
            urls.push(url.clone());
        }
        for i in 0..num_segments {
            if let Some(url) = self.playlist.segment_url(variant, i) {
                urls.push(url);
            }
        }
        if urls.is_empty() {
            return None;
        }

        let store = self.scope.store();
        let mut sizes: Vec<u64> = Vec::with_capacity(urls.len());
        let mut probe_slots: Vec<usize> = Vec::new();
        let mut cmds: Vec<FetchCmd> = Vec::new();
        for (slot, url) in urls.iter().enumerate() {
            let key = self.scope.key_from_url(url);
            if let Some(len) = store.final_len(&key).filter(|len| *len > 0) {
                sizes.push(len);
            } else {
                sizes.push(0);
                probe_slots.push(slot);
                cmds.push(self.probe_cmd(url.clone()));
            }
        }
        debug!(
            variant,
            resources = urls.len(),
            cached = urls.len() - cmds.len(),
            probes = cmds.len(),
            "size_estimation: cache-first split"
        );

        let mut results: Vec<_> = Vec::with_capacity(cmds.len());
        let mut remaining = cmds;
        while !remaining.is_empty() {
            let take = self.concurrency.min(remaining.len());
            let chunk: Vec<FetchCmd> = remaining.drain(..take).collect();
            results.extend(self.peer.batch(chunk).await);
        }
        for (slot, resp) in probe_slots.into_iter().zip(results.iter()) {
            sizes[slot] = resp.as_ref().ok().map_or(0, Self::content_length);
        }

        let mut iter = sizes.into_iter();
        let init_size = if init_url.is_some() {
            iter.next().unwrap_or(0)
        } else {
            0
        };
        let mut offsets = Vec::with_capacity(num_segments);
        let mut segment_sizes = Vec::with_capacity(num_segments);
        let mut cumulative = 0u64;
        for i in 0..num_segments {
            let media_len = iter.next().unwrap_or(0);
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
            init_size,
            total: cumulative,
        })
    }
}
