use kithara_platform::time::Duration;
use kithara_stream::SourceSeekAnchor;
use tracing::{debug, trace};

use super::{core::HlsSource, types::SeekLayout};
use crate::{HlsError, playlist::PlaylistAccess};

impl HlsSource {
    pub(super) fn resolve_seek_anchor(
        &self,
        position: Duration,
    ) -> Result<SourceSeekAnchor, HlsError> {
        let variants = self.playlist_state.num_variants();
        if variants == 0 {
            return Err(HlsError::SegmentNotFound("empty playlist".to_string()));
        }

        // Use current layout variant, NOT ABR target.
        // ABR must not affect seek resolution — variant switch happens
        // after seek completes, via format_change detection.
        let mut variant = self.segments.lock_sync().layout_variant();
        if variant >= variants {
            variant = 0;
        }

        let (segment_index, segment_start, segment_end) = self
            .playlist_state
            .find_seek_point_for_time(variant, position)
            .ok_or_else(|| {
                HlsError::SegmentNotFound(format!(
                    "seek point not found: variant={variant} target_ms={}",
                    position.as_millis()
                ))
            })?;

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
    /// Since `resolve_seek_anchor` always uses `layout_variant` (not ABR target),
    /// seeks are always within the current layout → always Preserve.
    /// Variant switches happen via `format_change` detection, not during seek.
    pub(super) fn classify_seek(&self, anchor: &SourceSeekAnchor) -> SeekLayout {
        let target_variant = anchor.variant_index.unwrap_or(0);
        match self.current_layout_variant() {
            Some(current) if current == target_variant => SeekLayout::Preserve,
            Some(current) if self.can_cross_variant_without_reset(current, target_variant) => {
                SeekLayout::Preserve
            }
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
