use std::ops::Range;

use kithara_bufpool::HasPool;
use kithara_stream::{SourcePhase, needs_exact_byte_sizes};

use crate::{segment::PlannedFetch, variant::HlsVariant};

pub(in crate::variant) enum RangeGate {
    Eof,
    Metadata(SourcePhase),
    Pending,
    Ready,
}

impl<S> HlsVariant<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Reads the membership mirror rather than the queue lock, since this runs on the produce core
    /// inside `phase_at`, where a blocking lock would spin into `sched_yield` under planner
    /// contention in a real-time context.
    pub(in crate::variant) fn fetch_is_planned(&self, planned: PlannedFetch) -> bool {
        self.flow.queue.planned(planned)
    }

    fn init_has_demand(&self) -> bool {
        self.init_downloading() || self.fetch_is_planned(PlannedFetch::Init)
    }

    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        self.phase_at_with(range, || {})
    }

    fn phase_at_with(&self, range: Range<u64>, after_eof: impl FnOnce()) -> SourcePhase {
        match self.range_gate_with(&range, after_eof) {
            Some(RangeGate::Eof) => SourcePhase::Eof,
            Some(RangeGate::Metadata(phase)) => phase,
            Some(RangeGate::Ready) => SourcePhase::Ready,
            Some(RangeGate::Pending) => self.range_wait_phase(&range),
            None => SourcePhase::WaitingDemand,
        }
    }

    pub(in crate::variant) fn range_gate(&self, range: &Range<u64>) -> Option<RangeGate> {
        self.range_gate_with(range, || {})
    }

    fn range_gate_with(&self, range: &Range<u64>, after_eof: impl FnOnce()) -> Option<RangeGate> {
        self.layout.try_published(|| {
            let uses_seek_alias = self.seek_alias_at(range.start).is_some();
            let total = self.total_bytes();
            let eof = !uses_seek_alias && self.eof_at_published(range.start, total);
            after_eof();
            if eof {
                return Some(RangeGate::Eof);
            }
            if let Some(phase) = self.exact_seek_metadata_phase() {
                return Some(RangeGate::Metadata(phase));
            }
            if let Some(phase) = self.exact_byte_metadata_phase() {
                return Some(RangeGate::Metadata(phase));
            }
            Some(if self.range_ready_published_with(range, || {}) {
                RangeGate::Ready
            } else {
                RangeGate::Pending
            })
        })
    }

    /// Returns whether a resource covering `range` settled terminally.
    pub(in crate::variant) fn range_has_failed(&self, range: &Range<u64>) -> bool {
        if self.init_prefix_is_unsized() && self.init_failed() {
            return true;
        }
        let total = self.total_bytes();
        let uses_seek_alias = self.seek_alias_at(range.start).is_some();
        let end = if !uses_seek_alias && total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        let mut cursor = range.start;
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

    #[cfg(test)]
    pub(crate) fn range_ready(&self, range: &Range<u64>) -> bool {
        self.range_ready_with(range, || {})
    }

    /// Treats an incomplete total as only a lower bound: treating it as EOF would admit a
    /// zero-width ready range before an unsized segment arrives.
    fn range_ready_published_with(&self, range: &Range<u64>, after_total: impl FnOnce()) -> bool {
        let total = self.total_bytes();
        after_total();
        let uses_seek_alias = self.seek_alias_at(range.start).is_some();
        let clamp_alias_to_eof = uses_seek_alias
            && !needs_exact_byte_sizes(self.profile.codec, self.profile.container)
            && self.eof_ready();
        if !uses_seek_alias && total > 0 && range.start >= total && !self.sizes_complete() {
            return false;
        }
        let end = if total > 0 && (!uses_seek_alias || clamp_alias_to_eof) {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return true;
        }
        if self.init_prefix_is_unsized() {
            return false;
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

    #[cfg(test)]
    fn range_ready_with(&self, range: &Range<u64>, after_total: impl FnOnce()) -> bool {
        self.layout
            .try_published(|| Some(self.range_ready_published_with(range, after_total)))
            .unwrap_or(false)
    }

    fn range_wait_phase(&self, range: &Range<u64>) -> SourcePhase {
        self.range_wait_phase_with(range, |_| {})
    }

    fn range_wait_phase_published(
        &self,
        range: &Range<u64>,
        mut on_demand: impl FnMut(PlannedFetch),
    ) -> SourcePhase {
        let total = self.total_bytes();
        let uses_seek_alias = self.seek_alias_at(range.start).is_some();
        let clamp_alias_to_eof = uses_seek_alias
            && !needs_exact_byte_sizes(self.profile.codec, self.profile.container)
            && self.eof_ready();
        if !uses_seek_alias && total > 0 && range.start >= total && !self.sizes_complete() {
            let head = self.download_head();
            return if self.segment_has_demand(head) {
                on_demand(PlannedFetch::Segment(head));
                SourcePhase::WaitingDemand
            } else {
                SourcePhase::Waiting
            };
        }

        let end = if total > 0 && (!uses_seek_alias || clamp_alias_to_eof) {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return SourcePhase::Waiting;
        }
        if self.init_prefix_is_unsized() {
            return if self.init_has_demand() {
                on_demand(PlannedFetch::Init);
                SourcePhase::WaitingDemand
            } else {
                SourcePhase::Waiting
            };
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
                if !self.init_has_demand() {
                    return SourcePhase::Waiting;
                }
                on_demand(PlannedFetch::Init);
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
                if !self.segment_has_demand(seg_idx) {
                    return SourcePhase::Waiting;
                }
                on_demand(PlannedFetch::Segment(seg_idx));
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

    /// Wait phase of `range`; `on_demand` sees every planned or in-flight
    /// fetch the range still needs bytes from. The query itself is pure —
    /// only the wait filing in `wait_range` passes a writing visitor.
    pub(in crate::variant) fn range_wait_phase_with(
        &self,
        range: &Range<u64>,
        on_demand: impl FnMut(PlannedFetch),
    ) -> SourcePhase {
        self.layout
            .try_published(|| Some(self.range_wait_phase_published(range, on_demand)))
            .unwrap_or(SourcePhase::WaitingDemand)
    }

    delegate::delegate! {
        to self {
            #[cfg(test)]
            #[call(phase_at_with)]
            pub(crate) fn phase_at_after_eof(
                &self,
                range: Range<u64>,
                after_eof: impl FnOnce(),
            ) -> SourcePhase;
            #[cfg(test)]
            #[call(range_ready_with)]
            pub(crate) fn range_ready_after_total(
                &self,
                range: &Range<u64>,
                after_total: impl FnOnce(),
            ) -> bool;
        }
    }
}
