use std::{
    num::NonZeroUsize,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_events::HlsEvent;
use kithara_platform::{
    thread::yield_now,
    time::{Duration, Instant},
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    MediaInfo, PendingReason, ReadOutcome, Source, SourcePhase, SourceSeekAnchor, StreamError,
    StreamResult, Timeline,
};
use kithara_test_utils::kithara;
use tracing::{debug, trace};

use super::{
    core::HlsSource,
    types::{ReadSegment, WAIT_RANGE_HANG_TIMEOUT_FLOOR, WAIT_RANGE_SLEEP_MS},
};
use crate::{HlsError, coord::SegmentRequest, ids::VariantIndex, playlist::PlaylistAccess};

fn wait_range_hang_timeout(timeout: Option<Duration>) -> Duration {
    timeout.map_or(WAIT_RANGE_HANG_TIMEOUT_FLOOR, |t| {
        t.max(WAIT_RANGE_HANG_TIMEOUT_FLOOR)
    })
}

/// `HlsError` is not `Clone` (transitive `kithara_net::NetError` /
/// `kithara_assets::AssetsError` are not), so the permanent-error path
/// rebuilds an equivalent variant from the stored `Arc<HlsError>`.
/// Only the variants the scheduler actually marks as permanent are
/// preserved verbatim — any leaked `Net`/`Storage`/`Assets` is surfaced
/// as `KeyProcessing` carrying the original `Display` so the reader
/// still gets an actionable message.
fn clone_hls_error(err: &HlsError) -> HlsError {
    match err {
        HlsError::KeyProcessing(s) => HlsError::KeyProcessing(s.clone()),
        HlsError::PlaylistParse(s) => HlsError::PlaylistParse(s.clone()),
        HlsError::VariantNotFound(s) => HlsError::VariantNotFound(s.clone()),
        HlsError::SegmentNotFound(s) => HlsError::SegmentNotFound(s.clone()),
        HlsError::InvalidUrl(s) => HlsError::InvalidUrl(s.clone()),
        HlsError::Cancelled => HlsError::Cancelled,
        HlsError::WaitBudgetExceeded => HlsError::WaitBudgetExceeded,
        HlsError::Timeout(s) => HlsError::Timeout(s.clone()),
        other => HlsError::KeyProcessing(format!("permanent: {other}")),
    }
}

/// Probe site fired the moment `wait_range` notices that the timeline's
/// `seek_epoch` advanced past the epoch it captured on entry. Lets
/// tests pin the bug fix from Phase 3: under the bug the same probe
/// would NOT precede a return-Interrupted, instead the loop fell into
/// Phase 3 demand push under the new epoch.
#[kithara::probe(prev_epoch, seek_epoch, is_seek_pending, range_start)]
fn record_wait_range_epoch_advance(
    prev_epoch: u64,
    seek_epoch: u64,
    is_seek_pending: bool,
    range_start: u64,
) {
    let _ = (prev_epoch, seek_epoch, is_seek_pending, range_start);
}

impl Source for HlsSource {
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self._peer_handle.as_ref().map(|h| h.abr().clone())
    }

    fn as_segment_layout(&self) -> Option<Arc<dyn kithara_stream::SegmentLayout>> {
        Some(Arc::clone(&self.segmented_view) as Arc<dyn kithara_stream::SegmentLayout>)
    }

    fn clear_variant_fence(&mut self) {
        self.variant_fence = None;
    }

    fn commit_seek_landing(&mut self, anchor: Option<SourceSeekAnchor>) {
        let seek_epoch = self.coord.timeline().seek_epoch();
        let landed_offset = self.coord.position();
        trace!(
            target: "hls_seek_diag",
            seek_epoch,
            landed_offset,
            anchor_segment = anchor.and_then(|a| a.segment_index).map(|i| i as usize),
            anchor_byte_offset = anchor.map(|a| a.byte_offset),
            "commit_seek_landing: enter"
        );
        let fallback_variant = anchor
            .and_then(|resolved| resolved.variant_index)
            .unwrap_or_else(|| self.resolve_current_variant());
        let Some((variant, segment_index)) =
            self.resolve_landed_segment(anchor, landed_offset, fallback_variant, seek_epoch)
        else {
            debug!(
                seek_epoch,
                variant = fallback_variant,
                landed_offset,
                "commit_seek_landing: could not resolve landed segment and no anchor fallback"
            );
            self.coord.condvar.notify_all();
            self.coord.reader_advanced.notify_one();
            return;
        };
        self.apply_landed_segment(variant, segment_index, landed_offset, seek_epoch);
    }

    fn commit_variant_layout(&mut self) {
        let target = self.coord.variant_index();
        let mut segments = self.segments.lock_sync();
        if segments.layout_variant() != target {
            segments.set_layout_variant(target);
            if let Some(sizes) = self.playlist_state.segment_sizes(target) {
                segments.set_expected_sizes(target, sizes);
            }
        }
    }

    fn current_segment_range(&self) -> Option<Range<u64>> {
        let (variant, seg_idx) = self.current_loaded_segment_key()?;
        let segments = self.segments.lock_sync();
        segments.item_range((variant, seg_idx))
    }

    fn demand_range(&self, range: Range<u64>) {
        if range.is_empty() {
            return;
        }
        let seek_epoch = self.coord.timeline().seek_epoch();
        self.queue_segment_request_for_offset(range.start, seek_epoch);
    }

    #[kithara::probe]
    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        let current_variant = self.coord.variant_index();

        if let Some(range) = self.init_segment_range_for_variant(current_variant).range {
            return Some(range);
        }

        let fallback_variant = {
            let segments = self.segments.lock_sync();
            let reader_offset = self.coord.position();
            segments
                .find_at_offset(reader_offset)
                .filter(|s| s.data.is_some())
                .map(|seg_ref| seg_ref.variant)
                .or_else(|| {
                    let max = segments.max_end_offset();
                    if max > 0 {
                        segments
                            .find_at_offset(max.saturating_sub(1))
                            .filter(|s| s.data.is_some())
                            .map(|seg_ref| seg_ref.variant)
                    } else {
                        None
                    }
                })
        };
        if let Some(fallback_variant) = fallback_variant
            && let Some(range) = self.init_segment_range_for_variant(fallback_variant).range
        {
            return Some(range);
        }

        let layout_floor = self.segments.lock_sync().layout_floor_segment();
        if let Some((variant, segment_index)) = layout_floor {
            return self.metadata_range_for_segment(variant, segment_index);
        }

        if current_variant >= self.playlist_state.num_variants() {
            return None;
        }
        self.metadata_range_for_segment(current_variant, 0)
    }

    fn len(&self) -> Option<u64> {
        self.effective_total_bytes()
    }

    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        let coord = Arc::clone(&self.coord);
        Some(Box::new(move || {
            coord.condvar.notify_all();
            coord.reader_advanced.notify_one();
        }))
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let hinted_variant = self.coord.variant_index();
        let reader_variant = self.current_loaded_segment_key().map(|(v, _)| v);
        let has_hinted_variant = self
            .segments
            .lock_sync()
            .variant_segments(hinted_variant)
            .is_some_and(|vs| !vs.is_empty());
        let variant = match reader_variant {
            Some(reader) => reader,
            None if has_hinted_variant => hinted_variant,
            None if hinted_variant < self.playlist_state.num_variants() => hinted_variant,
            None => return None,
        };
        let codec = self.playlist_state.variant_codec(variant);
        let container = self.playlist_state.variant_container(variant);
        let variant_u32 = u32::try_from(variant).unwrap_or(u32::MAX);
        Some(MediaInfo::new(codec, container).with_variant_index(variant_u32))
    }

    fn notify_waiting(&self) {
        self.coord.condvar.notify_all();
        self.coord.reader_advanced.notify_one();
    }

    fn phase(&self) -> SourcePhase {
        let pos = self.coord.position();

        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }

        let (segment_ready, past_eof) = {
            let segments = self.segments.lock_sync();
            let ready = segments.find_at_offset(pos).is_some_and(|seg_ref| {
                seg_ref.data.is_some_and(|data| {
                    let seg_range = seg_ref.byte_offset..seg_ref.byte_offset + data.total_len();
                    self.range_ready_from_segments(&segments, &seg_range)
                })
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

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        let segments = self.segments.lock_sync();
        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }
        if self.range_ready_from_segments(&segments, &range) {
            return SourcePhase::Ready;
        }
        if range.start >= segments.max_end_offset() && segments.max_end_offset() > 0 {
            let abr_variant = self.coord.variant_index();
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

    #[kithara::probe(offset)]
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let read_epoch = self.coord.timeline().seek_epoch();
        let (seg, effective_total) = {
            let segments = self.segments.lock_sync();
            let found = segments
                .visible_segment_at(offset)
                .and_then(|r| ReadSegment::try_from_ref(&r));
            (
                found,
                segments.effective_total(self.playlist_state.as_ref()),
            )
        };

        let Some(seg) = seg else {
            if effective_total > 0 && offset >= effective_total {
                return Ok(ReadOutcome::Eof);
            }

            let layout_variant = self.segments.lock_sync().layout_variant();
            let abr_variant = self.coord.variant_index();
            if abr_variant != layout_variant {
                trace!(
                    offset,
                    layout_variant,
                    abr_variant,
                    "read_at: ABR variant stall — signaling VariantChange"
                );
                return Ok(ReadOutcome::Pending(PendingReason::VariantChange));
            }

            self.queue_segment_request_for_offset(offset, read_epoch);
            return Ok(ReadOutcome::Pending(PendingReason::Retry));
        };

        let previous_hint = self.current_segment_index().unwrap_or(seg.segment_index);
        if seg.segment_index < previous_hint {
            self.bus.publish(HlsEvent::Seek {
                offset,
                stage: "read_at_moved_segment_backward",
                seek_epoch: self.coord.timeline().seek_epoch(),
                variant: seg.variant,
                from_segment_index: previous_hint,
                to_segment_index: seg.segment_index,
            });
        }

        if self.variant_fence.is_none() {
            self.variant_fence = Some(seg.variant);
        }
        if let Some(fence) = self.variant_fence
            && seg.variant != fence
        {
            if self.can_cross_variant_without_reset(fence, seg.variant) {
                self.variant_fence = Some(seg.variant);
            } else {
                return Ok(ReadOutcome::Pending(PendingReason::VariantChange));
            }
        }

        let Some(bytes) = self
            .read_from_entry(&seg, offset, buf)
            .map_err(|e| StreamError::Source(e.into()))?
        else {
            let seek_epoch = self.coord.timeline().seek_epoch();
            self.push_segment_request(seg.variant, seg.segment_index, seek_epoch);
            return Ok(ReadOutcome::Pending(PendingReason::Retry));
        };

        let Some(count) = NonZeroUsize::new(bytes) else {
            return Ok(ReadOutcome::Pending(PendingReason::Retry));
        };

        if self.coord.timeline().seek_epoch() != read_epoch {
            return Ok(ReadOutcome::Pending(PendingReason::Retry));
        }
        if seg.segment_index != previous_hint {
            self.coord.reader_advanced.notify_one();
        }

        self.reader_segment
            .store(seg.segment_index, Ordering::Release);

        Ok(ReadOutcome::Bytes(count))
    }

    fn seek_time_anchor(&mut self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        let anchor = self
            .resolve_seek_anchor(position)
            .map_err(|e| StreamError::Source(e.into()))?;
        let layout = self.classify_seek(&anchor);
        self.apply_seek_plan(&anchor, &layout);
        Ok(Some(anchor))
    }

    #[kithara_hang_detector::hang_watchdog]
    fn set_seek_epoch(&mut self, _seek_epoch: u64) {
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);

        self.coord.clear_segment_requests();

        self.coord.reader_advanced.notify_one();
        self.coord.condvar.notify_all();
    }

    fn take_reader_hooks(&mut self) -> Option<kithara_stream::SharedHooks> {
        let hooks = super::reader_hooks::HlsReaderHooks::new(
            self.bus.clone(),
            Arc::clone(&self.segments),
            self.coord.position_handle(),
            self.coord.timeline().seek_epoch_handle(),
        );
        Some(Arc::new(std::sync::Mutex::new(hooks)))
    }

    fn timeline(&self) -> Timeline {
        self.coord.timeline()
    }

    fn position(&self) -> u64 {
        self.coord.position()
    }

    fn advance(&self, n: u64) {
        self.coord.advance_position(n);
    }

    fn set_position(&self, pos: u64) {
        self.coord.set_position(pos);
    }

    fn position_handle(&self) -> Arc<AtomicU64> {
        self.coord.position_handle()
    }

    #[kithara_hang_detector::hang_watchdog(timeout = wait_range_hang_timeout(timeout))]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        let mut wait_seek_epoch: Option<u64> = None;
        let started_at = Instant::now();

        loop {
            if let Some(budget) = timeout
                && started_at.elapsed() > budget
            {
                return Err(StreamError::Source(HlsError::WaitBudgetExceeded.into()));
            }

            let seek_epoch;
            let range_ready;
            let cancelled;
            let stopped;
            let seeking;
            let past_eof;
            let permanent_error;
            {
                let segments = self.segments.lock_sync();
                seek_epoch = self.coord.timeline().seek_epoch();
                match wait_seek_epoch {
                    Some(epoch) if epoch != seek_epoch => {
                        let pending = self.coord.timeline().is_seek_pending();
                        record_wait_range_epoch_advance(epoch, seek_epoch, pending, range.start);
                        return Ok(WaitOutcome::Interrupted);
                    }
                    None => wait_seek_epoch = Some(seek_epoch),
                    _ => {}
                }

                cancelled = self.coord.cancel.is_cancelled();
                stopped = self.coord.stopped.load(Ordering::Acquire);
                range_ready = self.range_ready_from_segments(&segments, &range);
                seeking = self.coord.timeline().is_flushing();
                past_eof = self.is_past_eof(&segments, &range);
                permanent_error = (!range_ready)
                    .then(|| segments.find_failed_at_offset(range.start))
                    .flatten();

                if !range_ready && !cancelled && !stopped && !seeking && !past_eof {
                    trace!(
                        range_start = range.start,
                        range_end = range.end,
                        layout_variant = segments.layout_variant(),
                        found_seg = segments
                            .find_at_offset(range.start)
                            .map(|s| s.segment_index),
                        max_end = segments.max_end_offset(),
                        num_committed = segments.num_committed(),
                        elapsed_ms =
                            u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "wait_range: not ready"
                    );
                }
            }

            if range_ready {
                hang_reset!();
                self.coord
                    .had_midstream_switch
                    .store(false, Ordering::Release);
                return Ok(WaitOutcome::Ready);
            }
            if cancelled || (stopped && !range_ready) {
                return if stopped && past_eof {
                    Ok(WaitOutcome::Eof)
                } else {
                    Err(StreamError::Source(HlsError::Cancelled.into()))
                };
            }
            if let Some(err) = permanent_error {
                debug!(
                    range_start = range.start,
                    error = %err,
                    "wait_range: scheduler reported permanent failure"
                );
                return Err(StreamError::Source(clone_hls_error(&err).into()));
            }
            if seeking {
                return Ok(WaitOutcome::Interrupted);
            }
            if past_eof {
                debug!(range_start = range.start, "wait_range: EOF");
                return Ok(WaitOutcome::Eof);
            }

            if !self.queue_segment_request_for_offset(range.start, seek_epoch) {
                trace!(
                    range_start = range.start,
                    seek_epoch, "wait_range: metadata not yet available, waiting"
                );
            }

            {
                let segments = self.segments.lock_sync();
                hang_tick!();
                yield_now();
                let deadline = Instant::now() + Duration::from_millis(WAIT_RANGE_SLEEP_MS);
                let (_segments, _wait_result) =
                    self.coord.condvar.wait_sync_timeout(segments, deadline);
            }

            if self.coord.timeline().is_flushing() {
                return Ok(WaitOutcome::Interrupted);
            }
        }
    }
}

impl HlsSource {
    fn anchor_fallback(
        anchor: Option<SourceSeekAnchor>,
        landed_offset: u64,
        seek_epoch: u64,
    ) -> Option<(VariantIndex, usize)> {
        let anchor = anchor?;
        let variant = anchor.variant_index?;
        let segment_index = anchor.segment_index? as usize;
        debug!(
            seek_epoch,
            variant,
            segment_index,
            landed_offset,
            "commit_seek_landing: landed offset unresolvable, falling back to anchor"
        );
        Some((variant, segment_index))
    }

    fn apply_landed_segment(
        &mut self,
        variant: VariantIndex,
        segment_index: usize,
        landed_offset: u64,
        seek_epoch: u64,
    ) {
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
        self.coord.reader_advanced.notify_one();

        if previous_hint != segment_index {
            self.bus.publish(HlsEvent::Seek {
                seek_epoch,
                variant,
                stage: "seek_landing_set_segment",
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

    fn layout_match_shadowed_by_anchor(
        anchor: Option<SourceSeekAnchor>,
        landed_offset: u64,
        variant: VariantIndex,
        segment_index: usize,
    ) -> bool {
        anchor.is_some_and(|resolved| {
            let Some(anchor_variant) = resolved.variant_index else {
                return false;
            };
            let Some(anchor_segment) = resolved.segment_index.map(|index| index as usize) else {
                return false;
            };
            landed_offset < resolved.byte_offset
                && variant == anchor_variant
                && segment_index >= anchor_segment
        })
    }

    fn resolve_landed_segment(
        &self,
        anchor: Option<SourceSeekAnchor>,
        landed_offset: u64,
        fallback_variant: VariantIndex,
        seek_epoch: u64,
    ) -> Option<(VariantIndex, usize)> {
        let primary = self
            .layout_segment_for_offset(landed_offset)
            .filter(|&(variant, segment_index)| {
                !Self::layout_match_shadowed_by_anchor(
                    anchor,
                    landed_offset,
                    variant,
                    segment_index,
                )
            })
            .or_else(|| {
                self.resolve_segment_for_offset(landed_offset, fallback_variant)
                    .map(|seg_idx| (fallback_variant, seg_idx))
            });
        primary.or_else(|| Self::anchor_fallback(anchor, landed_offset, seek_epoch))
    }
}
