use kithara_stream::{SourcePhase, needs_exact_byte_sizes};
use tracing::trace;

use super::{HlsVariant, probe::SizeDemand};
use crate::segment::PlannedFetch;

#[derive(Clone, Copy)]
pub(super) struct ExactSeekDemand {
    pub(super) segment: u32,
    pub(super) anchor: u64,
}

impl HlsVariant {
    /// Media-and-init completeness. [`Self::sizes_complete`] is a MEDIA-only
    /// proxy (it ignores the `EXT-X-MAP` init); the exact-byte offset table
    /// seeds from the init size, so a non-exact init shifts every segment
    /// offset once it commits. Exact-byte seek short-circuits must fold the
    /// init in or a seek before the init resolves can skip the
    /// `SizeDemand::Init` request and land on stale bytes once the init
    /// commit shifts offsets.
    fn all_sizes_complete(&self) -> bool {
        self.sizes_complete() && self.exact_init_complete()
    }

    pub(super) fn clear_exact_byte_seek(&self) {
        // RT-reachable (via `exact_byte_metadata_phase`): a lock-free,
        // alloc-free store of the none sentinel.
        self.seek.exact_byte_seek.store(None);
    }

    pub(super) fn clear_exact_seek(&self) {
        self.seek.exact_seek.clear();
    }

    pub(super) fn complete_exact_seek_if_ready(&self) {
        let entry = {
            let Some(entry) = self.seek.exact_seek.load() else {
                return;
            };
            if !self.exact_prefix_complete(entry.segment) {
                return;
            }
            // Consume the demand only if the slot is still the exact generation
            // we validated. Losing the CAS means a concurrent completer (off-RT
            // settle or a fresh SET) already took it — `exact_prefix_complete`
            // is monotonic, so the winner runs the follow-up; we bail.
            if !self.seek.exact_seek.take_if(entry.generation) {
                return;
            }
            entry
        };
        let demand = ExactSeekDemand {
            segment: entry.segment,
            anchor: entry.anchor,
        };
        let Some(exact_anchor) = self.segment_byte_offset(demand.segment) else {
            return;
        };
        if self.flow.reader.position() == demand.anchor {
            // Keep the stale seek alias alive: audio still gates the
            // SourceSeekAnchor byte returned before exact prefix resolution.
            self.resolve_seek_alias(demand, exact_anchor);
            self.flow.reader.set_position(exact_anchor);
            self.set_prefetch_anchor(exact_anchor);
        }
    }

    fn enqueue_body_fetch_for_size(&self, demand: SizeDemand) {
        let planned = match demand {
            SizeDemand::Init => PlannedFetch::Init,
            SizeDemand::Segment(idx) => PlannedFetch::Segment(idx),
        };
        let mut queue = self.flow.queue.lock();
        if !queue.contains(&planned) {
            queue.push_front(planned);
        }
    }

    pub(super) fn exact_byte_metadata_phase(&self) -> Option<SourcePhase> {
        let byte = self.seek.exact_byte_seek.load()?;
        if self.exact_byte_position_ready(byte) {
            self.clear_exact_byte_seek();
            return None;
        }
        self.request_exact_prefix_for_byte(byte);
        Some(SourcePhase::WaitingDemand)
    }

    fn exact_byte_position_ready(&self, byte: u64) -> bool {
        if self.all_sizes_complete() {
            return true;
        }
        self.find_at_offset(byte)
            .is_some_and(|(segment, _, _)| self.exact_prefix_complete(segment))
    }

    fn exact_init_complete(&self) -> bool {
        self.segments
            .init
            .as_ref()
            .is_none_or(|init| init.size().is_exact())
    }

    fn exact_prefix_complete(&self, segment: u32) -> bool {
        if self
            .segments
            .init
            .as_ref()
            .is_some_and(|init| !init.size().is_exact())
        {
            return false;
        }
        let Some(end) = usize::try_from(segment)
            .ok()
            .map(|idx| idx.min(self.segments.len().saturating_sub(1)))
        else {
            return false;
        };
        self.segments
            .get(..=end)
            .is_some_and(|prefix| prefix.iter().all(|segment| segment.size().is_exact()))
    }

    pub(super) fn exact_seek_metadata_phase(&self) -> Option<SourcePhase> {
        let demand = self.seek.exact_seek.load()?;
        if self.exact_prefix_complete(demand.segment) {
            return None;
        }
        self.request_exact_prefix_through(demand.segment);
        Some(SourcePhase::WaitingDemand)
    }

    pub(crate) fn prepare_exact_prefix_for_boundary(&self, boundary: u32) -> bool {
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            return true;
        }
        let boundary = boundary.min(self.num_segments());
        if self
            .segments
            .init
            .as_ref()
            .is_some_and(|init| !init.size().is_exact())
        {
            self.request_exact_size(SizeDemand::Init);
        }
        let Some(last) = boundary.checked_sub(1) else {
            return self.exact_init_complete();
        };
        self.request_exact_prefix_through(last);
        self.exact_prefix_complete(last)
    }

    fn request_exact_prefix_for_byte(&self, byte: u64) {
        if let Some((segment, _, _)) = self.find_at_offset(byte) {
            self.request_exact_prefix_through(segment);
            return;
        }
        let last = self.num_segments().saturating_sub(1);
        if last > 0 || !self.segments.is_empty() {
            self.request_exact_prefix_through(last);
        }
    }

    fn request_exact_prefix_through(&self, segment: u32) {
        if self
            .segments
            .init
            .as_ref()
            .is_some_and(|init| !init.size().is_exact())
        {
            self.request_exact_size(SizeDemand::Init);
        }
        let last = segment.min(self.num_segments().saturating_sub(1));
        for idx in (0..=last).filter(|idx| {
            self.segments
                .get(*idx as usize)
                .is_some_and(|segment| !segment.size().is_exact())
        }) {
            self.request_exact_size(SizeDemand::Segment(idx));
        }
    }

    fn request_exact_size(&self, demand: SizeDemand) {
        trace!(
            variant = self.variant,
            demand = ?demand,
            probe_allowed = self.size_probe_allowed(demand),
            "request exact size"
        );
        if self.size_probe_allowed(demand) {
            self.enqueue_size_demand(demand);
        } else {
            self.enqueue_body_fetch_for_size(demand);
        }
    }

    pub(super) fn set_exact_byte_seek_demand(&self, byte: u64) {
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container)
            || self.all_sizes_complete()
        {
            self.clear_exact_byte_seek();
            return;
        }
        trace!(
            variant = self.variant,
            byte, "register exact byte seek demand"
        );
        self.seek.exact_byte_seek.store(Some(byte));
        self.request_exact_prefix_for_byte(byte);
    }

    pub(super) fn set_exact_seek_demand(&self, anchor: u64, segment: u32) {
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container)
            || self.all_sizes_complete()
        {
            // No exact-size demand to register: either the container resolves
            // ranges by segment index, or every served size *and* the init are
            // already exact, so the O(prefix) re-scan and recompute would be
            self.clear_exact_seek();
            return;
        }
        if self
            .seek
            .exact_seek
            .load()
            .is_some_and(|demand| demand.segment == segment)
        {
            self.request_exact_prefix_through(segment);
            self.complete_exact_seek_if_ready();
            return;
        }
        trace!(
            variant = self.variant,
            segment, anchor, "register exact seek demand"
        );
        self.seek.exact_seek.set(segment, anchor);
        self.request_exact_prefix_through(segment);
        self.complete_exact_seek_if_ready();
    }
}
