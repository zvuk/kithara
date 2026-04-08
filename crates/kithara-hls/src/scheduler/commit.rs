use std::sync::atomic::Ordering;

use kithara_events::HlsEvent;

use super::{helpers::first_missing_segment, state::HlsScheduler, trait_impl::HlsFetch};
use crate::stream_index::SegmentData;

impl HlsScheduler {
    pub(super) fn commit_segment(
        &mut self,
        dl: HlsFetch,
        _is_variant_switch: bool,
        _is_midstream_switch: bool,
    ) {
        let seg_idx = dl.segment.media_index().unwrap_or(0);

        self.record_throughput(dl.media.len, dl.duration, dl.media.duration);

        self.bus.publish(HlsEvent::SegmentComplete {
            variant: dl.variant,
            segment_index: seg_idx,
            bytes_transferred: dl.media.len,
            cached: dl.media_cached,
            duration: dl.duration,
        });

        let fresh_init_len = dl.init_len;

        let actual_init_len = if fresh_init_len == 0 {
            let segments = self.segments.lock_sync();
            segments
                .stored_segment(dl.variant, seg_idx)
                .map_or(0, |existing| existing.init_len)
        } else {
            fresh_init_len
        };

        let media_len = dl.media.len;
        let actual_size = actual_init_len + media_len;

        let init_url = if actual_init_len > 0 {
            dl.init_url.or_else(|| {
                let segments = self.segments.lock_sync();
                segments
                    .stored_segment(dl.variant, seg_idx)
                    .and_then(|existing| existing.init_url.clone())
            })
        } else {
            dl.init_url
        };

        let data = SegmentData {
            init_len: actual_init_len,
            media_len,
            init_url,
            media_url: dl.media.url.clone(),
        };

        self.segments
            .lock_sync()
            .commit_segment(dl.variant, seg_idx, data);

        let end_offset = self.segments.lock_sync().max_end_offset();
        let current_download = self.coord.timeline().download_position();
        let next_download = current_download.max(end_offset);
        self.coord.timeline().set_download_position(next_download);

        self.playlist_state
            .reconcile_segment_size(dl.variant, seg_idx, actual_size);

        if let Some(sizes) = self.playlist_state.segment_sizes(dl.variant) {
            self.segments
                .lock_sync()
                .set_expected_sizes(dl.variant, sizes);
        }

        self.bus.publish(HlsEvent::DownloadProgress {
            offset: next_download,
            total: None,
        });

        self.coord.condvar.notify_all();
    }

    pub(super) fn handle_midstream_switch(&mut self, is_midstream_switch: bool) {
        if !is_midstream_switch {
            return;
        }

        let old_variant = self.download_variant;
        let num_segments = self.num_segments(old_variant).unwrap_or(0);
        let cursor_pos = {
            let state = self.segments.lock_sync();
            first_missing_segment(&state, old_variant, 0, num_segments).unwrap_or(num_segments)
        };

        self.cursor.reopen_fill(cursor_pos, cursor_pos);
        self.coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        self.coord.clear_segment_requests();
        self.coord.condvar.notify_all();
    }
}
