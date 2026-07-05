use std::{
    collections::{BTreeSet, VecDeque},
    sync::Arc,
};

use kithara_net::Headers;
use kithara_stream::dl::{FetchCmd, OnCompleteFn, WriterFn};
use tracing::{debug, trace};
use url::Url;

use super::{HlsVariant, PlanCtx};
use crate::{
    handle::{SegmentPeer, segment_peer::parse_size_headers},
    segment::SegmentContent,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum SizeDemand {
    Init,
    Segment(u32),
}

#[derive(Default)]
pub(super) struct SizeDemandState {
    inflight: BTreeSet<SizeDemand>,
    queued: BTreeSet<SizeDemand>,
    queue: VecDeque<SizeDemand>,
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

    fn dispatchable_size_demand(&self) -> Option<SizeDemand> {
        self.seek.size_demand.lock().pop_dispatchable()
    }

    pub(super) fn enqueue_size_demand(&self, demand: SizeDemand) {
        self.seek.size_demand.lock().enqueue(demand);
    }

    fn finish_size_demand(&self, demand: SizeDemand) {
        self.seek.size_demand.lock().finish(demand);
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

    pub(super) fn size_probe_allowed(&self, demand: SizeDemand) -> bool {
        match demand {
            SizeDemand::Init => self.segments.init.as_ref().is_some_and(|segment| {
                !segment.size().is_exact() && matches!(segment.content(), SegmentContent::Plain)
            }),
            SizeDemand::Segment(idx) => self.segments.get(idx as usize).is_some_and(|segment| {
                !segment.size().is_exact() && matches!(segment.content(), SegmentContent::Plain)
            }),
        }
    }

    fn size_probe_url(&self, demand: SizeDemand) -> Option<Url> {
        match demand {
            SizeDemand::Init => Some(self.segments.init.as_ref()?.url().clone()),
            SizeDemand::Segment(idx) => Some(self.segments.get(idx as usize)?.url().clone()),
        }
    }
}
