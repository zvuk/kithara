use std::sync::atomic::Ordering;

use kithara_test_utils::kithara;

use super::{helpers::first_missing_segment, state::HlsScheduler};
use crate::{loading::SegmentMeta, playlist::PlaylistAccess, stream_index::SegmentData};

impl HlsScheduler {
    #[kithara::probe(variant, seg_idx, init_len, caller)]
    pub(super) fn commit_segment(
        &mut self,
        variant: usize,
        seg_idx: usize,
        media: &SegmentMeta,
        init_len: u64,
        init_url: Option<url::Url>,
        _duration: std::time::Duration,
    ) {
        let actual_init_len = if init_len == 0 {
            let segments = self.segments.lock_sync();
            segments
                .stored_segment(variant, seg_idx)
                .map_or(0, |existing| existing.init_len)
        } else {
            init_len
        };

        let media_len = media.len;
        let actual_size = actual_init_len + media_len;

        let expected_size = self
            .playlist_state
            .segment_size(variant, seg_idx)
            .unwrap_or(0);
        if expected_size != actual_size {
            tracing::debug!(
                variant,
                seg_idx,
                expected_size,
                actual_size,
                actual_init_len,
                media_len,
                delta = (actual_size as i64) - (expected_size as i64),
                "commit: size mismatch HEAD vs actual"
            );
        }

        let init_url = if actual_init_len > 0 {
            init_url.or_else(|| {
                let segments = self.segments.lock_sync();
                segments
                    .stored_segment(variant, seg_idx)
                    .and_then(|existing| existing.init_url.clone())
            })
        } else {
            init_url
        };

        let data = SegmentData {
            media_len,
            init_url,
            init_len: actual_init_len,
            media_url: media.url.clone(),
        };

        self.segments
            .lock_sync()
            .commit_segment(variant, seg_idx, data);

        self.committed_segment
            .fetch_max(seg_idx + 1, Ordering::AcqRel);

        let end_offset = self.segments.lock_sync().max_end_offset();
        let current_download = self.coord.timeline().download_position();
        let next_download = current_download.max(end_offset);
        self.coord.timeline().set_download_position(next_download);

        self.playlist_state
            .reconcile_segment_size(variant, seg_idx, actual_size);

        if let Some(sizes) = self.playlist_state.segment_sizes(variant) {
            self.segments.lock_sync().set_expected_sizes(variant, sizes);
        }

        self.coord.condvar.notify_all();
    }

    pub(crate) fn handle_midstream_switch(&mut self, is_midstream_switch: bool) {
        if !is_midstream_switch {
            return;
        }

        let old_variant = self.download_variant;
        let num_segments = self.num_segments(old_variant).unwrap_or(0);
        let reader_floor = self.reader_segment_floor();
        let cursor_pos = {
            let state = self.segments.lock_sync();
            first_missing_segment(
                &state,
                old_variant,
                reader_floor,
                num_segments,
                self.backend.is_ephemeral(),
            )
            .unwrap_or(num_segments)
        };

        self.runtime.cursor.reopen_fill(cursor_pos, cursor_pos);
        self.coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        self.coord.clear_segment_requests();
        self.coord.condvar.notify_all();
    }
}
