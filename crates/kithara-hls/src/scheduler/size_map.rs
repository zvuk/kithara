use kithara_assets::{AssetResourceState, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::Mutex;
use tracing::debug;

use super::state::HlsScheduler;
use crate::{
    coord::HlsCoord,
    playlist::{PlaylistAccess, PlaylistState},
    stream_index::{SegmentData, StreamIndex},
};

impl HlsScheduler {
    pub(crate) fn populate_cached_segments(
        segments: &Mutex<StreamIndex>,
        coord: &HlsCoord,
        backend: &AssetStore<DecryptContext>,
        playlist_state: &PlaylistState,
        variant: usize,
    ) -> (usize, u64) {
        if backend.is_ephemeral() {
            return (0, 0);
        }

        Self::populate_cached_segments_with_open(segments, coord, playlist_state, variant, |key| {
            backend.resource_state(key).ok()
        })
    }

    pub(crate) fn populate_cached_segments_with_open<F>(
        segments: &Mutex<StreamIndex>,
        coord: &HlsCoord,
        playlist_state: &PlaylistState,
        variant: usize,
        mut open_status: F,
    ) -> (usize, u64)
    where
        F: FnMut(&ResourceKey) -> Option<AssetResourceState>,
    {
        let init_url = playlist_state.init_url(variant);
        let num_segments = playlist_state.num_segments(variant).unwrap_or(0);

        if let Some(ref url) = init_url {
            let init_key = ResourceKey::from_url(url);
            let init_cached = open_status(&init_key)
                .is_some_and(|status| matches!(status, AssetResourceState::Committed { .. }));
            if !init_cached {
                return (0, 0);
            }
        }

        #[expect(
            clippy::option_if_let_else,
            reason = "nested conditionals are clearer with if-let"
        )]
        let init_len = if playlist_state.total_variant_size(variant).is_some() {
            if let Some(ref url) = init_url {
                let key = ResourceKey::from_url(url);
                open_status(&key)
                    .and_then(|status| match status {
                        AssetResourceState::Committed { final_len } => final_len,
                        _ => None,
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };

        let mut count = 0usize;

        for index in 0..num_segments {
            let Some(segment_url) = playlist_state.segment_url(variant, index) else {
                break;
            };

            let key = ResourceKey::from_url(&segment_url);
            let Some(status) = open_status(&key) else {
                break;
            };

            if let AssetResourceState::Committed { final_len } = status {
                let media_len = final_len.unwrap_or(0);
                if media_len == 0 {
                    break;
                }

                let (mut actual_init_len, mut seg_init_url) = if count == 0 {
                    (init_len, init_url.clone())
                } else {
                    (0, None)
                };

                {
                    let segs = segments.lock_sync();
                    if let Some(existing) = segs.stored_segment(variant, index) {
                        if existing.init_len > actual_init_len {
                            actual_init_len = existing.init_len;
                        }
                        if actual_init_len > 0 && seg_init_url.is_none() {
                            seg_init_url = existing.init_url.clone();
                        }
                    }
                }

                let data = SegmentData {
                    init_len: actual_init_len,
                    media_len,
                    init_url: seg_init_url,
                    media_url: segment_url,
                };

                segments.lock_sync().commit_segment(variant, index, data);
                count += 1;
            } else {
                break;
            }
        }

        let cumulative_offset = segments.lock_sync().max_end_offset();
        if count > 0 {
            debug!(
                variant,
                count, cumulative_offset, "pre-populated cached segments from disk"
            );
            coord.condvar.notify_all();
        }

        (count, cumulative_offset)
    }

    pub(crate) fn populate_cached_segments_if_needed(&mut self, variant: usize) -> (usize, u64) {
        // Steady-state short-circuit: once the disk scan has discovered
        // every committed segment for this variant, re-scanning is a no-op
        // that re-commits identical `SegmentData` and fires
        // `coord.condvar.notify_all()` — which wakes the audio worker, which
        // re-polls, which calls this again. That feedback loop is what
        // makes `live_stress_real_stream_seek_read_cache_drm_mmap` exceed
        // its 60s budget under stress.
        //
        // The tracker is only meaningful for non-ephemeral backends:
        // ephemeral stores short-circuit `populate_cached_segments`
        // directly (no durable cache to scan). For non-ephemeral backends
        // there is no LRU eviction, so a previously-populated count
        // cannot become stale — fresh segments only ever push the count
        // forward, and the live-playlist case (`num_segments` grows)
        // re-enters the slow path because `prev_count < num_segments`.
        if self.backend.is_ephemeral() {
            return (0, 0);
        }
        let num_segments = self.playlist_state.num_segments(variant).unwrap_or(0);
        if num_segments > 0
            && self
                .populated_cached_count
                .get(&variant)
                .copied()
                .is_some_and(|prev| prev >= num_segments)
        {
            let cumulative_offset = self.segments.lock_sync().max_end_offset();
            return (num_segments, cumulative_offset);
        }

        let (count, cumulative_offset) = Self::populate_cached_segments(
            &self.segments,
            &self.coord,
            &self.backend,
            &self.playlist_state,
            variant,
        );
        self.populated_cached_count.insert(variant, count);
        (count, cumulative_offset)
    }

    pub(crate) fn apply_cached_segment_progress(
        &mut self,
        variant: usize,
        cached_count: usize,
        cached_end_offset: u64,
    ) {
        if cached_count == 0 {
            return;
        }

        let current_download = self.coord.timeline().download_position();
        if cached_end_offset > current_download {
            self.coord
                .timeline()
                .set_download_position(cached_end_offset);
        }
        if cached_count > self.current_segment_index() {
            self.advance_current_segment_index(cached_count);
        }
        self.sent_init_for_variant.insert(variant);

        let already_announced = self
            .announced_cached_count
            .get(&variant)
            .copied()
            .unwrap_or(0);
        if cached_count <= already_announced {
            return;
        }

        for seg_idx in already_announced..cached_count {
            let bytes = self
                .segments
                .lock_sync()
                .stored_segment(variant, seg_idx)
                .map_or(0, |s| s.media_len + s.init_len);
            self.bus.publish(kithara_events::HlsEvent::SegmentComplete {
                variant,
                segment_index: seg_idx,
                bytes_transferred: bytes,
                cached: true,
                duration: std::time::Duration::ZERO,
            });
        }
        self.announced_cached_count.insert(variant, cached_count);
    }
}
