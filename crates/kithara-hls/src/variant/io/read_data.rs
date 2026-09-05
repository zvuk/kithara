use std::{num::NonZeroUsize, ops::Range};

use kithara_bufpool::HasPool;
use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{PendingReason, ReadOutcome, StreamError, StreamResult};
use kithara_test_utils::kithara;
use tracing::trace;

use super::{HlsVariant, read::RangeGate};
use crate::{
    HlsError,
    segment::{PlannedFetch, Segment},
};

impl<S> HlsVariant<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    #[kithara::hang_watchdog]
    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let uses_seek_alias = self.seek_alias_at(offset).is_some();
        if !uses_seek_alias && self.eof_at(offset) {
            return Ok(ReadOutcome::Eof);
        }
        if self.exact_seek_metadata_phase().is_some() || self.exact_byte_metadata_phase().is_some()
        {
            trace!(
                variant = self.variant,
                offset, "read_at: gated by exact-size metadata demand"
            );
            return Ok(Self::wrap(0));
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

        // WHY: A fresh unsized `#EXT-X-MAP` still reserves offset zero; serving
        // media there would replace the required container header.
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
            let Some(n) = self.segment_read_at(seg_idx, local_start..local_end, dst)? else {
                trace!(
                    variant = self.variant,
                    seg_idx,
                    cursor,
                    size_exact = self
                        .segments
                        .get(seg_idx as usize)
                        .is_some_and(|s| s.size().is_exact()),
                    loaded = self.segment_loaded(seg_idx),
                    "read_at: segment bytes unavailable"
                );
                break;
            };
            written += n;
            cursor += n as u64;
            if n < take {
                trace!(
                    variant = self.variant,
                    seg_idx, cursor, n, take, "read_at: short segment read"
                );
                break;
            }
        }

        Ok(Self::wrap(written))
    }

    /// Asked only for a segment a wait found no bytes for, so the probe names
    /// the exact slot state behind a starved wait: a gap with `loaded=1` is a
    /// state/storage split, one with all-zero flags is work that fell off the
    /// plan.
    #[kithara::probe(
        variant = self.variant as u64,
        seg = u64::from(seg_idx),
        loaded = u64::from(self.segment_loaded(seg_idx)),
        downloading = u64::from(self.segment_downloading(seg_idx)),
        failed = u64::from(self.segment_failed(seg_idx)),
        planned = u64::from(self.fetch_is_planned(PlannedFetch::Segment(seg_idx)))
    )]
    pub(super) fn segment_has_demand(&self, seg_idx: u32) -> bool {
        self.segment_downloading(seg_idx) || self.fetch_is_planned(PlannedFetch::Segment(seg_idx))
    }

    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        self.wait_range_with(range, |planned| self.note_fetch_demand(planned))
    }

    /// Readiness poll of a session under construction. Parks like a wait but
    /// files no demand: the poll is not a read, and an incoming variant's
    /// fetches are owed already.
    pub(crate) fn poll_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome> {
        self.wait_range_with(range, |_| {})
    }

    #[kithara::hang_watchdog]
    fn wait_range_with(
        &self,
        range: Range<u64>,
        on_demand: impl FnMut(PlannedFetch),
    ) -> StreamResult<WaitOutcome> {
        let stable_pending = match self.range_gate(&range) {
            Some(RangeGate::Eof) => {
                self.flow.reader.clear_wait();
                return Ok(WaitOutcome::Eof);
            }
            Some(RangeGate::Ready) => {
                hang_reset!();
                self.flow.reader.clear_wait();
                return Ok(WaitOutcome::Ready);
            }
            Some(RangeGate::Metadata(_) | RangeGate::Pending) => true,
            None => false,
        };
        if self.flow.reader.is_flushing() {
            self.flow.reader.clear_wait();
            return Ok(WaitOutcome::Interrupted);
        }
        if stable_pending && self.range_has_failed(&range) {
            self.flow.reader.clear_wait();
            return Err(StreamError::Source(HlsError::SegmentUnavailable.into()));
        }
        self.flow.reader.note_wait(range.end);
        let phase = self.range_wait_phase_with(&range, on_demand);
        trace!(
            variant = self.variant,
            start = range.start,
            end = range.end,
            ?phase,
            "wait_range: range not ready (budget exceeded)"
        );
        Err(StreamError::Source(HlsError::WaitBudgetExceeded.into()))
    }

    /// File the parked read on a fetch it needs bytes from, so the in-flight
    /// command's demand probe escalates it in the downloader queue.
    fn note_fetch_demand(&self, planned: PlannedFetch) {
        let state = match planned {
            PlannedFetch::Init => self.segments.init.as_ref().map(Segment::state),
            PlannedFetch::Segment(idx) => self.segments.get(idx as usize).map(Segment::state),
        };
        if let Some(state) = state {
            state.note_reader_demand();
        }
    }

    fn wrap(written: usize) -> ReadOutcome {
        NonZeroUsize::new(written).map_or(
            ReadOutcome::Pending(PendingReason::Retry),
            ReadOutcome::Bytes,
        )
    }
}
