use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

use super::{HlsVariant, PlanCtx, SegmentActivateParams, offsets::ActivateParams};
use crate::segment::PlannedFetch;

impl HlsVariant {
    /// Auto-mode switch activation. Two byte positions matter:
    ///
    /// - `seg_boundary` — the **virtual** byte where this variant's
    ///   segment `from_seg` should start in the combined stream. The
    ///   caller should pass the outgoing variant's segment boundary
    ///   (e.g. `v_old.segment_byte_offset(from_seg)`) so the byte
    ///   address space joins without gaps or overlaps; box scans across
    ///   the boundary stay correctly aligned.
    /// - `reader_pos` — the reader's current source position. Stored as
    ///   the new variant's `position` so `coord.position()` stays
    ///   monotonic after the active variant flips. May be `>= seg_boundary`
    ///   when the reader had already partially read into `from_seg`'s
    ///   byte range from the outgoing variant before the commit fired.
    ///
    /// `byte_shift` is derived from `seg_boundary`, not `reader_pos`, so
    /// segment offsets pin to actual fMP4 box boundaries.
    pub(crate) fn activate_at_segment_with_shift(
        &self,
        ctx: &PlanCtx,
        params: SegmentActivateParams,
    ) {
        let SegmentActivateParams {
            from_seg,
            seg_boundary,
            reader_pos,
        } = params;
        self.rearm_cancel();
        let from_seg = from_seg.min(self.num_segments());
        self.layout.activate_with_shift(
            ActivateParams {
                from_seg,
                seg_boundary,
                init_size: self.init_route_size(),
            },
            &self.segments,
        );
        self.set_position_without_byte_demand(reader_pos);
        self.rebuild(ctx, from_seg);
    }

    /// Index of the first non-`Loaded` segment — interpreted as the
    /// "download head" by the ABR controller. Returns `num_segments()`
    /// when every segment is `Loaded`. Scans linearly; cheap because it
    /// only runs from `Abr::progress` (ABR tick cadence).
    pub(crate) fn download_head(&self) -> u32 {
        let head = self
            .segments
            .iter()
            .position(|s| !s.state().is_loaded())
            .unwrap_or(self.segments.len());
        u32::try_from(head).unwrap_or(u32::MAX)
    }

    /// True when every fetch a `rebuild_queue(from_seg)` would enqueue is
    /// already `Loaded` — the init (if any) plus every media segment in
    /// `[from_seg, num_segments)`. In that state a rebuild only reseeds
    /// entries `dispatch` immediately skips, so it is pure churn on a
    /// fully-cached seek. A partial cache returns `false` and rebuilds, so
    /// the prefetch tail is still re-aimed at the seek target whenever a
    /// fetch is actually outstanding.
    pub(super) fn fetch_plan_satisfied(&self, from_seg: u32) -> bool {
        if self.needs_init_fetch() {
            return false;
        }
        let segs_len = self.num_segments();
        (from_seg..segs_len).all(|idx| {
            self.segments
                .get(idx as usize)
                .is_some_and(|seg| seg.state().is_loaded())
        })
    }

    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.flow.queue.lock().len() as u64
    )]
    pub(crate) fn rebuild(&self, _ctx: &PlanCtx, from_seg: u32) {
        if self.fetch_plan_satisfied(from_seg) {
            return;
        }
        self.rebuild_queue(from_seg);
    }

    pub(crate) fn rebuild_at_time(&self, ctx: &PlanCtx, target: Duration) -> Option<u32> {
        let seg = self.segment_index_at_time(target)?;
        let fetch_start = self.seek_readahead_start_segment(seg);
        if let Some(byte) = self.segment_byte_offset(fetch_start) {
            self.set_prefetch_anchor(byte);
        }
        self.rebuild(ctx, fetch_start);
        if let Some(byte) = self.segment_byte_offset(seg) {
            self.set_exact_seek_demand(byte, seg);
        }
        Some(seg)
    }

    #[kithara::probe]
    pub(super) fn rebuild_queue(&self, from_seg: u32) {
        let segs_len = self.num_segments();
        let init = self
            .needs_init_fetch()
            .then_some(PlannedFetch::Init)
            .into_iter();
        let tail = (from_seg..segs_len).map(PlannedFetch::Segment);
        let mut queue = self.flow.queue.lock();
        queue.clear();
        queue.extend(init.chain(tail));
    }

    /// Same as [`Self::rebuild`] but also enqueues `seg 0` when
    /// `from_seg > 0`, so the decoder factory's probe has the container
    /// header to construct the codec. See the crate `CONTEXT.md`
    /// "Decoder-probe rebuild".
    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.flow.queue.lock().len() as u64
    )]
    pub(crate) fn rebuild_with_decoder_probe(&self, _ctx: &PlanCtx, from_seg: u32) {
        let from_seg = self.seek_readahead_start_segment(from_seg);
        self.set_segment_aware_seek_tail(from_seg);
        let segs_len = self.num_segments();
        let init = self
            .needs_init_fetch()
            .then_some(PlannedFetch::Init)
            .into_iter();
        let probe_seg = (from_seg > 0)
            .then_some(PlannedFetch::Segment(0))
            .into_iter();
        let tail = (from_seg..segs_len).map(PlannedFetch::Segment);
        let mut queue = self.flow.queue.lock();
        queue.clear();
        queue.extend(init.chain(probe_seg).chain(tail));
    }
}
