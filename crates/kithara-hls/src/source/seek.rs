use kithara_platform::time::Duration;
use kithara_stream::SourceSeekAnchor;
use tracing::{debug, trace};

use super::{core::HlsSource, types::SeekLayout};
use crate::{HlsError, playlist::PlaylistAccess, stream_index::VariantSegments};

impl HlsSource {
    pub(super) fn resolve_seek_anchor(
        &self,
        position: Duration,
    ) -> Result<SourceSeekAnchor, HlsError> {
        let variants = self.playlist_state.num_variants();
        if variants == 0 {
            return Err(HlsError::SegmentNotFound("empty playlist".to_string()));
        }

        // Default policy: anchor resolves in `layout_variant` so an
        // in-place seek doesn't uselessly recreate the decoder while
        // the layout still has the data. Fallback to `abr_variant_index`
        // fires ONLY when the layout is *stranded* — the target segment
        // lies strictly past the highest committed segment index in the
        // layout variant AND ABR has already moved on. That's the
        // silvercomet / kithara-app shape: ABR up-switched mid-playback,
        // peer stopped fetching the old variant, user seeks into a
        // region the peer will never deliver. Without fallback,
        // `wait_range` hangs forever on bytes that never arrive.
        //
        // We deliberately do NOT fall back on LRU-evicted holes inside
        // the layout's fetched range: peer is still actively engaged
        // with the layout variant (ABR is a *download target hint*,
        // not an *abandonment signal*), so the missing segment will be
        // re-fetched by the scheduler's tail/gap logic. Falling back
        // there would force a decoder recreation per evicted seek
        // target and pile up latency under stress-seek workloads.
        let layout_variant = {
            let segs = self.segments.lock_sync();
            let v = segs.layout_variant();
            if v < variants { v } else { 0 }
        };

        let (layout_segment_index, layout_segment_start, layout_segment_end) = self
            .playlist_state
            .find_seek_point_for_time(layout_variant, position)
            .ok_or_else(|| {
                HlsError::SegmentNotFound(format!(
                    "seek point not found: variant={layout_variant} target_ms={}",
                    position.as_millis()
                ))
            })?;

        // "Stranded" is stronger than "not loaded": the layout has
        // literally never fetched a segment at or after the target
        // index. We use `max_stored_index` (which includes segments
        // the LRU evicted) so that a cache miss on a previously
        // fetched segment does NOT trip the fallback — the scheduler
        // will re-fetch it.
        let (layout_has_target, layout_stranded) = {
            let segs = self.segments.lock_sync();
            let has_target = segs.is_segment_loaded(layout_variant, layout_segment_index);
            let max_stored = segs
                .variant_segments(layout_variant)
                .and_then(VariantSegments::max_stored_index);
            drop(segs);
            let stranded = !has_target && max_stored.is_none_or(|max| layout_segment_index > max);
            (has_target, stranded)
        };

        let (variant, segment_index, segment_start, segment_end) =
            if layout_has_target || !layout_stranded {
                (
                    layout_variant,
                    layout_segment_index,
                    layout_segment_start,
                    layout_segment_end,
                )
            } else {
                let abr_variant = self
                    .coord
                    .abr_variant_index
                    .load(std::sync::atomic::Ordering::Acquire);
                let fallback = if abr_variant < variants && abr_variant != layout_variant {
                    self.playlist_state
                        .find_seek_point_for_time(abr_variant, position)
                        .map(|(idx, start, end)| (abr_variant, idx, start, end))
                } else {
                    None
                };
                fallback.unwrap_or((
                    layout_variant,
                    layout_segment_index,
                    layout_segment_start,
                    layout_segment_end,
                ))
            };

        let byte_offset = self
            .byte_offset_for_segment(variant, segment_index)
            .ok_or_else(|| {
                HlsError::SegmentNotFound(format!(
                    "seek offset not found: variant={variant} segment={}",
                    segment_index
                ))
            })?;

        #[expect(clippy::cast_possible_truncation, reason = "segment index fits in u32")]
        let segment_index = segment_index as u32;
        Ok(SourceSeekAnchor::new(byte_offset, segment_start)
            .with_segment_end(segment_end)
            .with_segment_index(segment_index)
            .with_variant_index(variant))
    }

    /// Classify a seek as Preserve or Reset.
    ///
    /// Preserve only when the anchor stays inside the current layout variant.
    /// Cross-variant seek is always Reset — byte spaces are per-variant and
    /// non-convertible, so a shared codec is not enough: Preserve would leave
    /// the layout pinned at the old variant while `anchor.byte_offset` lives
    /// in the new variant's space, stranding `wait_range` on a segment that
    /// will never be fetched (post-ABR-switch seek hang).
    pub(super) fn classify_seek(&self, anchor: &SourceSeekAnchor) -> SeekLayout {
        let target_variant = anchor.variant_index.unwrap_or(0);
        match self.current_layout_variant() {
            Some(current) if current == target_variant => SeekLayout::Preserve,
            _ => SeekLayout::Reset,
        }
    }

    /// Apply the seek plan: update positions, optionally clear segments.
    pub(super) fn apply_seek_plan(&mut self, anchor: &SourceSeekAnchor, layout: &SeekLayout) {
        let variant = anchor.variant_index.unwrap_or(0);
        let segment_index = anchor.segment_index.unwrap_or(0) as usize;
        let seek_epoch = self.coord.timeline().seek_epoch();
        let previous_hint = self.current_segment_index().unwrap_or(0);

        // Always: drain stale requests. The authoritative post-seek demand
        // is issued later from `commit_seek_landing(...)` once the decoder
        // tells us where it actually landed.
        self.coord.clear_segment_requests();

        match *layout {
            SeekLayout::Preserve => {
                // Keep segments — byte layout valid, decoder seeks in place.
                // layout_variant stays unchanged. ABR switch (if pending) is
                // handled after seek via format_change detection.
                trace!(
                    seek_epoch,
                    variant,
                    segment_index,
                    byte_offset = anchor.byte_offset,
                    "seek plan: Preserve — keeping StreamIndex"
                );
            }
            SeekLayout::Reset => {
                // Switch layout variant — decoder will be recreated.
                let mut segments = self.segments.lock_sync();
                segments.set_layout_variant(variant);
                // Sync expected sizes for the target variant so
                // rebuild_variant_byte_map can reserve correct gap offsets.
                if let Some(sizes) = self.playlist_state.segment_sizes(variant) {
                    segments.set_expected_sizes(variant, sizes);
                }
                drop(segments);
                self.coord.timeline().set_download_position(0);
                debug!(
                    seek_epoch,
                    variant,
                    segment_index,
                    byte_offset = anchor.byte_offset,
                    "seek plan: Reset — switched layout variant"
                );
            }
        }

        // Do not commit reader position here. The authoritative post-seek
        // byte position is whatever the decoder actually lands on after
        // `decoder.seek(...)` drives the underlying `Read + Seek` stream.
        self.coord.reader_advanced.notify_one();
        self.coord.condvar.notify_all();

        if previous_hint != segment_index {
            self.bus.publish(kithara_events::HlsEvent::Seek {
                stage: "seek_anchor_set_hint",
                seek_epoch,
                variant,
                offset: anchor.byte_offset,
                from_segment_index: previous_hint,
                to_segment_index: segment_index,
            });
        }

        trace!(
            seek_epoch,
            target_ms = ?anchor.segment_start,
            variant,
            segment_index,
            byte_offset = anchor.byte_offset,
            "seek_time_anchor: resolved seek anchor"
        );
    }
}
