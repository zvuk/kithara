use std::{
    collections::{BTreeSet, VecDeque},
    sync::Arc,
};

use kithara_net::Headers;
use kithara_stream::{
    SourcePhase,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
    needs_exact_byte_sizes,
};
use tracing::{debug, trace};
use url::Url;

use super::{HlsVariant, PlanCtx};
use crate::{
    handle::{SegmentPeer, segment_peer::parse_size_headers},
    segment::{PlannedFetch, SegmentContent},
};

#[derive(Clone, Copy)]
pub(super) struct ExactSeekDemand {
    pub(super) segment: u32,
    pub(super) anchor: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum SizeDemand {
    Init,
    Segment(u32),
}

#[derive(Default)]
pub(super) struct SizeDemandState {
    queue: VecDeque<SizeDemand>,
    queued: BTreeSet<SizeDemand>,
    inflight: BTreeSet<SizeDemand>,
}

impl SizeDemandState {
    fn enqueue(&mut self, demand: SizeDemand) {
        if self.queued.contains(&demand) || self.inflight.contains(&demand) {
            return;
        }
        self.queued.insert(demand);
        self.queue.push_back(demand);
    }

    fn finish(&mut self, demand: SizeDemand) {
        self.inflight.remove(&demand);
    }

    fn pop_dispatchable(&mut self) -> Option<SizeDemand> {
        while let Some(demand) = self.queue.pop_front() {
            self.queued.remove(&demand);
            if self.inflight.insert(demand) {
                return Some(demand);
            }
        }
        None
    }
}

impl HlsVariant {
    pub(crate) fn dispatch_size_only(
        self: &Arc<Self>,
        ctx: &PlanCtx,
        budget: usize,
    ) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        let mut remaining = budget;
        self.dispatch_size_demands(ctx, &mut out, &mut remaining);
        out
    }

    pub(super) fn dispatch_size_demands(
        self: &Arc<Self>,
        ctx: &PlanCtx,
        out: &mut Vec<FetchCmd>,
        remaining: &mut usize,
    ) {
        while *remaining > 0 {
            let Some(demand) = self.dispatchable_size_demand() else {
                break;
            };
            if self.apply_cached_size(demand) {
                self.finish_size_demand(demand);
                ctx.signal.fire();
                trace!(
                    variant = self.variant,
                    demand = ?demand,
                    "resolved exact size from committed cache"
                );
                continue;
            }
            if !self.size_probe_allowed(demand) {
                self.finish_size_demand(demand);
                trace!(
                    variant = self.variant,
                    demand = ?demand,
                    "skip exact size probe"
                );
                continue;
            }
            let Some(cmd) = self.build_size_probe_cmd(ctx, demand) else {
                self.finish_size_demand(demand);
                trace!(
                    variant = self.variant,
                    demand = ?demand,
                    "failed to build exact size probe"
                );
                continue;
            };
            trace!(
                variant = self.variant,
                demand = ?demand,
                remaining = *remaining,
                "dispatch exact size probe"
            );
            out.push(cmd);
            *remaining -= 1;
        }
    }

    fn dispatchable_size_demand(&self) -> Option<SizeDemand> {
        self.seek.size_demand.lock().pop_dispatchable()
    }

    fn enqueue_size_demand(&self, demand: SizeDemand) {
        self.seek.size_demand.lock().enqueue(demand);
    }

    fn finish_size_demand(&self, demand: SizeDemand) {
        self.seek.size_demand.lock().finish(demand);
    }

    pub(super) fn clear_exact_seek(&self) {
        *self.seek.exact_seek.lock() = None;
    }

    pub(super) fn clear_exact_byte_seek(&self) {
        *self.seek.exact_byte_seek.lock() = None;
    }

    fn finish_size_probe(
        &self,
        demand: SizeDemand,
        headers: Option<&Headers>,
        err: Option<&kithara_net::NetError>,
    ) {
        self.finish_size_demand(demand);
        if let Some(err) = err {
            debug!(
                variant = self.variant,
                demand = ?demand,
                error = %err,
                "lazy size probe failed"
            );
            return;
        }
        let size = headers.map_or(0, parse_size_headers);
        if size == 0 {
            debug!(
                variant = self.variant,
                demand = ?demand,
                "lazy size probe completed without length"
            );
            return;
        }
        trace!(
            variant = self.variant,
            demand = ?demand,
            size,
            "lazy size probe resolved length"
        );
        self.apply_resolved_size(demand, size);
    }

    fn size_probe_url(&self, demand: SizeDemand) -> Option<Url> {
        match demand {
            SizeDemand::Init => Some(self.segments.init.as_ref()?.url().clone()),
            SizeDemand::Segment(idx) => Some(self.segments.get(idx as usize)?.url().clone()),
        }
    }

    fn size_probe_allowed(&self, demand: SizeDemand) -> bool {
        match demand {
            SizeDemand::Init => self.segments.init.as_ref().is_some_and(|segment| {
                !segment.size().is_exact() && matches!(segment.content(), SegmentContent::Plain)
            }),
            SizeDemand::Segment(idx) => self.segments.get(idx as usize).is_some_and(|segment| {
                !segment.size().is_exact() && matches!(segment.content(), SegmentContent::Plain)
            }),
        }
    }

    fn build_size_probe_cmd(
        self: &Arc<Self>,
        ctx: &PlanCtx,
        demand: SizeDemand,
    ) -> Option<FetchCmd> {
        let url = self.size_probe_url(demand)?;
        let cancel = self.cancel_handle();
        let weak = Arc::downgrade(self);
        let signal = ctx.signal.clone();
        let writer: WriterFn = Box::new(|_| Ok(()));
        let on_complete: OnCompleteFn = Box::new(move |_bytes_written, headers, err| {
            if let Some(variant) = weak.upgrade() {
                variant.finish_size_probe(demand, headers, err);
            }
            signal.fire();
        });
        let segment_peer = SegmentPeer::new(self.profile.headers.clone());
        Some(segment_peer.size_probe(url, ctx.size_probe_method, cancel, writer, on_complete))
    }

    fn apply_cached_size(&self, demand: SizeDemand) -> bool {
        let len = match demand {
            SizeDemand::Init => self.init_committed_final_len(),
            SizeDemand::Segment(idx) => self.committed_final_len(idx),
        };
        len.filter(|n| *n > 0)
            .is_some_and(|n| self.apply_resolved_size(demand, n))
    }

    pub(super) fn apply_resolved_size(&self, demand: SizeDemand, len: u64) -> bool {
        if len == 0 {
            return false;
        }
        let mut changed = false;
        self.layout.apply_commit(&self.segments, || {
            changed = match demand {
                SizeDemand::Init => self
                    .segments
                    .init
                    .as_ref()
                    .filter(|segment| matches!(segment.content(), SegmentContent::Plain))
                    .is_some_and(|segment| segment.set_resolved_size(len)),
                SizeDemand::Segment(idx) => self
                    .segments
                    .get(idx as usize)
                    .filter(|segment| matches!(segment.content(), SegmentContent::Plain))
                    .is_some_and(|segment| segment.set_resolved_size(len)),
            };
            self.init_route_size()
        });
        if changed {
            self.complete_exact_seek_if_ready();
        }
        changed
    }

    pub(super) fn set_exact_seek_demand(&self, anchor: u64, segment: u32) {
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            self.clear_exact_seek();
            return;
        }
        if self
            .seek
            .exact_seek
            .lock()
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
        *self.seek.exact_seek.lock() = Some(ExactSeekDemand { segment, anchor });
        self.request_exact_prefix_through(segment);
        self.complete_exact_seek_if_ready();
    }

    pub(super) fn set_exact_byte_seek_demand(&self, byte: u64) {
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container)
            || self.sizes_complete()
        {
            self.clear_exact_byte_seek();
            return;
        }
        trace!(
            variant = self.variant,
            byte, "register exact byte seek demand"
        );
        *self.seek.exact_byte_seek.lock() = Some(byte);
        self.request_exact_prefix_for_byte(byte);
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

    pub(super) fn exact_seek_metadata_phase(&self) -> Option<SourcePhase> {
        let demand = (*self.seek.exact_seek.lock())?;
        if self.exact_prefix_complete(demand.segment) {
            return None;
        }
        self.request_exact_prefix_through(demand.segment);
        Some(SourcePhase::WaitingDemand)
    }

    pub(super) fn exact_byte_metadata_phase(&self) -> Option<SourcePhase> {
        let byte = (*self.seek.exact_byte_seek.lock())?;
        if self.exact_byte_position_ready(byte) {
            self.clear_exact_byte_seek();
            return None;
        }
        self.request_exact_prefix_for_byte(byte);
        Some(SourcePhase::WaitingDemand)
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

    fn exact_byte_position_ready(&self, byte: u64) -> bool {
        if self.sizes_complete() {
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

    pub(super) fn complete_exact_seek_if_ready(&self) {
        let demand = {
            let mut guard = self.seek.exact_seek.lock();
            let Some(demand) = *guard else {
                return;
            };
            if !self.exact_prefix_complete(demand.segment) {
                return;
            }
            *guard = None;
            demand
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
}
