use std::{
    ops::Range,
    sync::{Arc, atomic::Ordering},
};

use kithara_events::HlsEvent;
use kithara_platform::{
    thread::yield_now,
    time::{Duration, Instant},
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    MediaInfo, ReadOutcome, Source, SourcePhase, SourceSeekAnchor, StreamError, StreamResult,
};
use tracing::{debug, trace};

use super::{
    core::HlsSource,
    types::{
        DemandRequestOutcome, ReadSegment, WAIT_RANGE_HANG_TIMEOUT_FLOOR,
        WAIT_RANGE_MAX_METADATA_MISS_SPINS, WAIT_RANGE_SLEEP_MS,
    },
};
use crate::{
    HlsError,
    coord::{HlsCoord, SegmentRequest},
    playlist::PlaylistAccess,
};

fn wait_range_hang_timeout(timeout: Duration) -> Duration {
    timeout.max(WAIT_RANGE_HANG_TIMEOUT_FLOOR)
}

impl Source for HlsSource {
    type Error = HlsError;
    type Coord = Arc<HlsCoord>;
    type Demand = SegmentRequest;

    fn coord(&self) -> &Self::Coord {
        &self.coord
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "lock must be held for condvar wait"
    )]
    #[kithara_hang_detector::hang_watchdog(timeout = wait_range_hang_timeout(timeout))]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, HlsError> {
        let mut metadata_miss_count: usize = 0;
        let mut wait_seek_epoch: Option<u64> = None;
        let started_at = Instant::now();

        loop {
            let mut segments = self.segments.lock_sync();
            let seek_epoch = self.coord.timeline().seek_epoch();
            match wait_seek_epoch {
                Some(epoch) if epoch != seek_epoch => {
                    metadata_miss_count = 0;
                    wait_seek_epoch = Some(seek_epoch);
                    if !self.coord.timeline().is_seek_pending() {
                        return Ok(WaitOutcome::Interrupted);
                    }
                }
                None => wait_seek_epoch = Some(seek_epoch),
                _ => {}
            }

            // Compute phase inline — priority order matches SourcePhase::classify.
            let cancelled = self.coord.cancel.is_cancelled();
            let stopped = self.coord.stopped.load(Ordering::Acquire);
            let range_ready = self.range_ready_from_segments(&segments, &range);
            let seeking = self.coord.timeline().is_flushing();
            let past_eof = self.is_past_eof(&segments, &range);
            let waiting_demand = self.coord.has_pending_segment_request(seek_epoch);
            let waiting_metadata = metadata_miss_count > 0 && !waiting_demand;

            if range_ready {
                hang_reset!();
            }

            // Priority dispatch (highest wins).
            if cancelled || (stopped && !range_ready) {
                return if stopped && past_eof {
                    Ok(WaitOutcome::Eof)
                } else {
                    Err(StreamError::Source(HlsError::Cancelled))
                };
            }
            if range_ready {
                self.coord
                    .had_midstream_switch
                    .store(false, Ordering::Release);
                return Ok(WaitOutcome::Ready);
            }
            if seeking {
                return Ok(WaitOutcome::Interrupted);
            }
            if past_eof {
                debug!(
                    range_start = range.start,
                    max_end = segments.max_end_offset(),
                    known_total = segments.effective_total(self.playlist_state.as_ref()),
                    num_committed = segments.num_committed(),
                    "wait_range: EOF"
                );
                return Ok(WaitOutcome::Eof);
            }

            // Waiting sub-states — log and continue spinning.
            let phase = if waiting_metadata {
                SourcePhase::WaitingMetadata
            } else if waiting_demand {
                SourcePhase::WaitingDemand
            } else {
                SourcePhase::Waiting
            };
            trace!(
                range_start = range.start,
                range_end = range.end,
                ?phase,
                "wait_range: spinning"
            );

            drop(segments);

            // Re-issue the demand on each spin so a stale pending request for
            // the same epoch gets replaced by the segment that owns this range.
            // During a stale EOF + zero-total reset window, demand resolution
            // can legitimately observe an empty layout before fresh metadata
            // arrives; treat that as a transient gap instead of a terminal miss.
            match self.request_on_demand_segment(range.start, seek_epoch) {
                DemandRequestOutcome::Requested { .. } | DemandRequestOutcome::TransientGap => {
                    metadata_miss_count = 0;
                }
                DemandRequestOutcome::MetadataMiss { variant, reason } => {
                    self.metadata_miss(
                        range.start,
                        seek_epoch,
                        variant,
                        &mut metadata_miss_count,
                        WAIT_RANGE_MAX_METADATA_MISS_SPINS,
                        reason.label(),
                    )
                    .map_err(|error| StreamError::Source(HlsError::SegmentNotFound(error)))?;
                }
            }

            // Honour the overall time budget.  Placed after decision logic
            // and on-demand requests so that errors (e.g. SegmentNotFound)
            // always take priority over the timeout fallback.
            if started_at.elapsed() > timeout {
                return Err(StreamError::Source(HlsError::Timeout(format!(
                    "wait_range budget exceeded: range={}..{} elapsed={:?} timeout={timeout:?}",
                    range.start,
                    range.end,
                    started_at.elapsed(),
                ))));
            }

            segments = self.segments.lock_sync();

            hang_tick!();
            yield_now();
            let deadline = Instant::now() + Duration::from_millis(WAIT_RANGE_SLEEP_MS);
            let (_segments, _wait_result) =
                self.coord.condvar.wait_sync_timeout(segments, deadline);

            if self.coord.timeline().is_flushing() {
                return Ok(WaitOutcome::Interrupted);
            }
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "segments guard used across multiple checks"
    )]
    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        let segments = self.segments.lock_sync();
        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }
        if self.range_ready_from_segments(&segments, &range) {
            return SourcePhase::Ready;
        }
        // ABR variant transition stall: decoder reads from layout_variant, but
        // the downloader switched to a different variant. If range_start is past
        // all committed data for layout_variant, data will never arrive.
        // Report Ready so read_at can detect VariantChange.
        if range.start >= segments.max_end_offset() && segments.max_end_offset() > 0 {
            let abr_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
            if abr_variant != segments.layout_variant() {
                return SourcePhase::Ready;
            }
        }
        if self.coord.timeline().is_flushing() {
            return SourcePhase::Seeking;
        }
        if self.is_past_eof(&segments, &range) {
            return SourcePhase::Eof;
        }
        SourcePhase::Waiting
    }

    fn phase(&self) -> SourcePhase {
        let pos = self.coord.timeline().byte_position();

        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }

        let (segment_ready, past_eof) = {
            let segments = self.segments.lock_sync();
            let ready = segments.find_at_offset(pos).is_some_and(|seg_ref| {
                let seg_range = seg_ref.byte_offset..seg_ref.byte_offset + seg_ref.data.total_len();
                self.range_ready_from_segments(&segments, &seg_range)
            });
            let eof = self.is_past_eof(&segments, &(pos..pos.saturating_add(1)));
            drop(segments);
            (ready, eof)
        };

        if segment_ready {
            return SourcePhase::Ready;
        }
        if self.coord.timeline().is_flushing() {
            return SourcePhase::Seeking;
        }
        if past_eof {
            return SourcePhase::Eof;
        }

        SourcePhase::Waiting
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, HlsError> {
        let read_epoch = self.coord.timeline().seek_epoch();
        let (seg, effective_total) = {
            let segments = self.segments.lock_sync();
            let found = segments
                .visible_segment_at(offset)
                .map(|r| ReadSegment::from_ref(&r));
            (
                found,
                segments.effective_total(self.playlist_state.as_ref()),
            )
        };

        let Some(seg) = seg else {
            if effective_total > 0 && offset >= effective_total {
                return Ok(ReadOutcome::Data(0));
            }

            // ABR variant transition stall detection:
            // Decoder reads from layout_variant's byte map, but data will never
            // arrive if the downloader switched to a different variant.
            // Signal VariantChange so the FSM recreates the decoder.
            let layout_variant = self.segments.lock_sync().layout_variant();
            let abr_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
            if abr_variant != layout_variant {
                trace!(
                    offset,
                    layout_variant,
                    abr_variant,
                    "read_at: ABR variant stall — signaling VariantChange"
                );
                return Ok(ReadOutcome::VariantChange);
            }

            self.queue_segment_request_for_offset(offset, read_epoch);
            return Ok(ReadOutcome::Retry);
        };

        let previous_hint = self.current_segment_index().unwrap_or(seg.segment_index);
        if seg.segment_index < previous_hint {
            self.bus.publish(HlsEvent::Seek {
                stage: "read_at_moved_segment_backward",
                seek_epoch: self.coord.timeline().seek_epoch(),
                variant: seg.variant,
                offset,
                from_segment_index: previous_hint,
                to_segment_index: seg.segment_index,
            });
        }

        // Variant fence: auto-detect on first read, block cross-variant reads.
        if self.variant_fence.is_none() {
            self.variant_fence = Some(seg.variant);
        }
        if let Some(fence) = self.variant_fence
            && seg.variant != fence
        {
            if self.can_cross_variant_without_reset(fence, seg.variant) {
                self.variant_fence = Some(seg.variant);
            } else {
                return Ok(ReadOutcome::VariantChange);
            }
        }

        let Some(bytes) = self
            .read_from_entry(&seg, offset, buf)
            .map_err(StreamError::Source)?
        else {
            // Resource evicted. Push an on-demand request so the downloader
            // re-fetches this segment even when it's at the tail (Idle state).
            let seek_epoch = self.coord.timeline().seek_epoch();
            self.push_segment_request(seg.variant, seg.segment_index, seek_epoch);
            return Ok(ReadOutcome::Retry);
        };

        if bytes > 0 {
            if self.coord.timeline().seek_epoch() != read_epoch {
                return Ok(ReadOutcome::Retry);
            }
            let new_pos = offset.saturating_add(bytes as u64);
            if seg.segment_index != previous_hint {
                self.coord.reader_advanced.notify_one();
            }

            let total = self.segments.lock_sync().max_end_offset();
            self.bus.publish(HlsEvent::ByteProgress {
                position: new_pos,
                total: Some(total),
            });
        }

        Ok(ReadOutcome::Data(bytes))
    }

    fn len(&self) -> Option<u64> {
        self.effective_total_bytes()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let hinted_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
        let reader_variant = self.current_loaded_segment_key().map(|(v, _)| v);
        let has_hinted_variant = self
            .segments
            .lock_sync()
            .variant_segments(hinted_variant)
            .is_some_and(|vs| !vs.is_empty());
        let variant = match reader_variant {
            Some(reader) if reader == hinted_variant => reader,
            Some(_reader) if self.variant_fence.is_some() && has_hinted_variant => hinted_variant,
            Some(reader) => reader,
            None if has_hinted_variant => hinted_variant,
            None if hinted_variant < self.playlist_state.num_variants() => hinted_variant,
            None => return None,
        };
        let codec = self.playlist_state.variant_codec(variant);
        let container = self.playlist_state.variant_container(variant);
        #[expect(clippy::cast_possible_truncation, reason = "variant index fits in u32")]
        Some(MediaInfo::new(codec, container).with_variant_index(variant as u32))
    }

    fn current_segment_range(&self) -> Option<Range<u64>> {
        let (variant, seg_idx) = self.current_loaded_segment_key()?;
        let segments = self.segments.lock_sync();
        segments.item_range((variant, seg_idx))
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        let current_variant = self.coord.abr_variant_index.load(Ordering::Acquire);

        // Do NOT change layout_variant here — this method is called from
        // detect_format_change() while the old decoder is still reading.
        // Switching layout now would make the old decoder read from the
        // wrong variant's byte map, corrupting Symphonia's moof table.
        //
        // Layout change happens later in commit_variant_layout(), called
        // from step_recreating_decoder() right before building the new
        // decoder.

        if let Some(range) = self.init_segment_range_for_variant(current_variant) {
            return Some(range);
        }

        let fallback_variant = {
            let segments = self.segments.lock_sync();
            let reader_offset = self.coord.timeline().byte_position();
            segments
                .find_at_offset(reader_offset)
                .map(|seg_ref| seg_ref.variant)
                .or_else(|| {
                    // Fallback: last committed segment
                    let max = segments.max_end_offset();
                    if max > 0 {
                        segments
                            .find_at_offset(max.saturating_sub(1))
                            .map(|seg_ref| seg_ref.variant)
                    } else {
                        None
                    }
                })
        };
        if let Some(fallback_variant) = fallback_variant
            && let Some(range) = self.init_segment_range_for_variant(fallback_variant)
        {
            return Some(range);
        }

        // After seek flush, no segments may be loaded yet.
        // Fall back to metadata for the first logical segment in the
        // current applied layout rather than variant segment 0.
        let layout_floor = self.segments.lock_sync().layout_floor_segment();
        if let Some((variant, segment_index)) = layout_floor {
            return self.metadata_range_for_segment(variant, segment_index);
        }

        if current_variant >= self.playlist_state.num_variants() {
            return None;
        }
        self.metadata_range_for_segment(current_variant, 0)
    }

    fn clear_variant_fence(&mut self) {
        self.variant_fence = None;
    }

    fn commit_variant_layout(&mut self) {
        let target = self.coord.abr_variant_index.load(Ordering::Acquire);
        let mut segments = self.segments.lock_sync();
        if segments.layout_variant() != target {
            segments.set_layout_variant(target);
            if let Some(sizes) = self.playlist_state.segment_sizes(target) {
                segments.set_expected_sizes(target, sizes);
            }
        }
    }

    fn notify_waiting(&self) {
        self.coord.condvar.notify_all();
        self.coord.reader_advanced.notify_one();
    }

    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        let coord = Arc::clone(&self.coord);
        Some(Box::new(move || {
            coord.condvar.notify_all();
            coord.reader_advanced.notify_one();
        }))
    }

    #[kithara_hang_detector::hang_watchdog]
    fn set_seek_epoch(&mut self, _seek_epoch: u64) {
        // Non-destructive: does NOT clear StreamIndex or download_position.
        // seek_time_anchor → classify_seek → apply_seek_plan handles that
        // conditionally based on SeekLayout (Preserve vs Reset).
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);

        self.coord.clear_segment_requests();

        self.coord.reader_advanced.notify_one();
        self.coord.condvar.notify_all();
    }

    fn seek_time_anchor(
        &mut self,
        position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>, HlsError> {
        let anchor = self
            .resolve_seek_anchor(position)
            .map_err(StreamError::Source)?;
        let layout = self.classify_seek(&anchor);
        self.apply_seek_plan(&anchor, &layout);
        Ok(Some(anchor))
    }

    fn commit_seek_landing(&mut self, anchor: Option<SourceSeekAnchor>) {
        let seek_epoch = self.coord.timeline().seek_epoch();
        let landed_offset = self.coord.timeline().byte_position();
        let fallback_variant = anchor
            .and_then(|resolved| resolved.variant_index)
            .unwrap_or_else(|| self.resolve_current_variant());
        let landed_segment = self
            .layout_segment_for_offset(landed_offset)
            .filter(|&(variant, segment_index)| {
                !anchor.is_some_and(|resolved| {
                    let Some(anchor_variant) = resolved.variant_index else {
                        return false;
                    };
                    let Some(anchor_segment) = resolved.segment_index.map(|index| index as usize)
                    else {
                        return false;
                    };
                    landed_offset < resolved.byte_offset
                        && variant == anchor_variant
                        && segment_index >= anchor_segment
                })
            })
            .or_else(|| {
                self.resolve_segment_for_offset(landed_offset, fallback_variant)
                    .map(|resolved| (fallback_variant, Self::segment_index(resolved)))
            });
        // `apply_seek_plan(...)` already established the authoritative layout
        // for this seek. Rewriting `variant_map` again at the landed offset
        // collapses mixed auto-switch layouts during replay from the prefix.
        let Some((variant, segment_index)) = landed_segment else {
            debug!(
                seek_epoch,
                variant = fallback_variant,
                landed_offset,
                "commit_seek_landing: could not resolve landed segment"
            );
            self.coord.condvar.notify_all();
            self.coord.reader_advanced.notify_one();
            return;
        };
        self.variant_fence = Some(variant);
        let segment_ready = self.loaded_segment_ready(variant, segment_index);

        let previous_hint = self.current_segment_index().unwrap_or(segment_index);
        self.coord.clear_segment_requests();
        if !segment_ready {
            self.coord.enqueue_segment_request(SegmentRequest {
                segment_index,
                variant,
                seek_epoch,
            });
        }
        self.coord.condvar.notify_all();

        if previous_hint != segment_index {
            self.bus.publish(HlsEvent::Seek {
                stage: "seek_landing_set_segment",
                seek_epoch,
                variant,
                offset: landed_offset,
                from_segment_index: previous_hint,
                to_segment_index: segment_index,
            });
        }

        trace!(
            seek_epoch,
            variant,
            segment_index,
            landed_offset,
            queued = !segment_ready,
            "commit_seek_landing: reconciled landed segment"
        );
    }

    fn demand_range(&self, range: Range<u64>) {
        if range.is_empty() {
            return;
        }
        let seek_epoch = self.coord.timeline().seek_epoch();
        self.queue_segment_request_for_offset(range.start, seek_epoch);
    }
}
