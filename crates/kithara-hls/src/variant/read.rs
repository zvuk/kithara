use std::{num::NonZeroUsize, ops::Range};

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{PendingReason, ReadOutcome, SourcePhase, StreamError, StreamResult};
use kithara_test_utils::kithara;

use super::HlsVariant;
use crate::HlsError;

impl HlsVariant {
    #[kithara::hang_watchdog]
    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let total = self.total_bytes();
        if total > 0 && offset >= total && self.sizes_complete() {
            return Ok(ReadOutcome::Eof);
        }

        let buf_len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let mut written: usize = 0;
        let mut cursor = offset;
        let read_end = offset.saturating_add(buf_len);

        while let Some(init_range) = self.init_descriptor_at(cursor) {
            hang_tick!();
            if cursor >= init_range.end {
                break;
            }
            let slice_end = read_end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self.init_read_at(local_start..local_end, dst)? {
                Some(n) => {
                    written += n;
                    cursor += n as u64;
                    if n < take {
                        return Ok(Self::wrap(written));
                    }
                    if cursor >= read_end {
                        return Ok(Self::wrap(written));
                    }
                }
                None => return Ok(Self::wrap(written)),
            }
        }

        // An `#EXT-X-MAP` init occupies the virtual prefix `[0, init_size)`.
        // While the init is declared (`has_init`) but not yet sized
        // (`init_size() == 0` — a failed/absent init HEAD, or the window before
        // the init commits), the offset table transiently seeds segment 0 at
        // offset 0. Serving media here would hand the demuxer segment 0's
        // container where the init's `ftyp`/`moov` belongs
        // ("re_mp4: ftyp not found"), or wedge the reader. Hold the read
        // pending: `needs_init_fetch` keeps the init enqueued and its commit
        // sizes the prefix, after which `init_descriptor_at` routes offset 0
        // to the init. Only the fresh-activation frame (`served_from() == 0`)
        // places the init at offset 0; a switched-in variant's init is
        // orphaned in natural space (see `init_descriptor_at`), so its reads
        // continue past offset 0 and must not be gated here. A terminally
        // failed init (`init_failed`) stops reserving the prefix so the read
        // surfaces an error instead of waiting forever.
        if self.has_init()
            && self.init_size() == 0
            && self.served_from() == 0
            && !self.init_failed()
        {
            return Ok(Self::wrap(written));
        }

        while cursor < read_end {
            hang_tick!();
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                break;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = read_end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self.segment_read_at(seg_idx, local_start..local_end, dst)? {
                Some(n) => {
                    written += n;
                    cursor += n as u64;
                    if n < take {
                        break;
                    }
                }
                None => break,
            }
        }

        Ok(Self::wrap(written))
    }

    #[kithara::hang_watchdog]
    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        // EOF must win over `range_ready`'s zero-width "ready" at the stream
        // end: a read at `range.start == total` clamps to a `[total, total)`
        // range that `range_ready` reports ready, so the gate would mint
        // `Ready`, `read_at` then returns `Eof`, and the consumer's
        // `phase()` stays `Ready` forever — EOF never becomes observable and a
        // reader polling on phase spins. A seek in flight may pull the
        // position back into the stream, so let the flush path win first.
        let total = self.total_bytes();
        if total > 0 && range.start >= total && self.sizes_complete() && !self.reader.is_flushing()
        {
            return Ok(WaitOutcome::Eof);
        }
        if self.range_ready(&range) {
            hang_reset!();
            return Ok(WaitOutcome::Ready);
        }
        if self.reader.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }
        // A segment covering this range settled terminally (the downloader
        // exhausted its retries): the bytes will never arrive, so surface a
        // terminal error instead of `WaitBudgetExceeded` — the reader stops
        // here rather than spinning. Checked AFTER flushing/EOF: a seek in
        // flight may be moving the read position off the failed range, and
        // the failed check is scoped to the requested range so seeking to a
        if self.range_has_failed(&range) {
            return Err(StreamError::Source(HlsError::SegmentUnavailable.into()));
        }
        // Not ready: the reader driver (`Stream::probe_read` / `read` /
        // `prime_seek_range`) wakes the peer for this range, per its own
        // on-core/off-core context — this method stays wake-free.
        Err(StreamError::Source(HlsError::WaitBudgetExceeded.into()))
    }

    fn wrap(written: usize) -> ReadOutcome {
        NonZeroUsize::new(written).map_or(
            ReadOutcome::Pending(PendingReason::Retry),
            ReadOutcome::Bytes,
        )
    }

    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        // EOF wins over `range_ready`'s zero-width "ready" at `range.start ==
        // total` so the phase is observable as `Eof` at the stream end (see
        // `wait_range`); a flush in flight takes precedence.
        let total = self.total_bytes();
        if total > 0 && range.start >= total && self.sizes_complete() && !self.reader.is_flushing()
        {
            return SourcePhase::Eof;
        }
        if self.range_ready(&range) {
            return SourcePhase::Ready;
        }
        if self.reader.is_flushing() {
            return SourcePhase::Seeking;
        }
        self.range_wait_phase(&range)
    }

    /// Whether any init/media segment covering `range` settled terminally
    /// (`Failed`): the downloader exhausted its retry budget, so the range
    /// will never load. [`wait_range`](Self::wait_range) consults this when
    /// a range is not ready to tell "still downloading" (spin) from
    /// "permanently failed" (terminal error). Walks the same descriptors as
    /// [`range_ready`](Self::range_ready), checking slot state rather than
    /// on-disk bytes; the per-byte `contains_range` walk stays out so this
    /// only fires on a real terminal settle, never on a transient gap.
    fn range_has_failed(&self, range: &Range<u64>) -> bool {
        let total = self.total_bytes();
        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        let mut cursor = range.start;
        // The init prefix is not a media segment, so `find_at_offset` returns
        // `None` for a byte inside it — skip past it (jumping to media space)
        // after checking the init's own terminal state, exactly as
        // `range_ready` walks init then media.
        if let Some(init_range) = self.init_descriptor_at(cursor) {
            if self.init_failed() {
                return true;
            }
            cursor = init_range.end;
        }
        while cursor < end {
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                break;
            };
            if self.segment_failed(seg_idx) {
                return true;
            }
            cursor = (seg_off + seg_size).max(cursor + 1);
        }
        false
    }

    pub(crate) fn range_ready(&self, range: &Range<u64>) -> bool {
        let total = self.total_bytes();
        // When a served segment's size is still unknown, `total` is a lower
        // bound, not the stream end. An offset at/past it is NOT "ready"
        // (clamping `end` to the under-count would falsely report a zero-width
        // ready range and let the reader spin past a real, not-yet-sized
        // segment) — treat it as not-ready so the gate holds Waiting.
        if total > 0 && range.start >= total && !self.sizes_complete() {
            return false;
        }
        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return true;
        }

        let mut cursor = range.start;
        while let Some(init_range) = self.init_descriptor_at(cursor) {
            if cursor >= init_range.end {
                break;
            }
            let slice_end = end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            if !self.init_contains(local_start..local_end) {
                return false;
            }
            cursor = slice_end;
            if cursor >= end {
                return true;
            }
        }
        if cursor >= end {
            return true;
        }

        while cursor < end {
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                return false;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            if !self.segment_contains(seg_idx, local_start..local_end) {
                return false;
            }
            cursor = slice_end;
        }
        cursor >= end
    }

    fn range_wait_phase(&self, range: &Range<u64>) -> SourcePhase {
        let total = self.total_bytes();
        if total > 0 && range.start >= total && !self.sizes_complete() {
            let head = self.download_head();
            return if self.segment_downloading(head) {
                SourcePhase::WaitingDemand
            } else {
                SourcePhase::Waiting
            };
        }

        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return SourcePhase::Waiting;
        }

        let mut waiting_on_demand = false;
        let mut cursor = range.start;
        while let Some(init_range) = self.init_descriptor_at(cursor) {
            if cursor >= init_range.end {
                break;
            }
            let slice_end = end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            if !self.init_contains(local_start..local_end) {
                if !self.init_downloading() {
                    return SourcePhase::Waiting;
                }
                waiting_on_demand = true;
            }
            cursor = slice_end;
            if cursor >= end {
                return if waiting_on_demand {
                    SourcePhase::WaitingDemand
                } else {
                    SourcePhase::Waiting
                };
            }
        }

        while cursor < end {
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                return SourcePhase::Waiting;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            if !self.segment_contains(seg_idx, local_start..local_end) {
                if !self.segment_downloading(seg_idx) {
                    return SourcePhase::Waiting;
                }
                waiting_on_demand = true;
            }
            cursor = slice_end;
        }

        if waiting_on_demand {
            SourcePhase::WaitingDemand
        } else {
            SourcePhase::Waiting
        }
    }
}
