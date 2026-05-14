#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara_abr::AbrHandle;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::{
    RwLock,
    time::{Duration, Instant},
};
use kithara_stream::{SegmentDescriptor, SegmentLayout, Timeline};
use tokio_util::sync::CancellationToken;

use crate::variant::{HlsVariant, PlanCtx};

/// Infrastructure handles shared with every [`HlsCoord`]:
/// the parent cancel token (cancel hierarchy owner of `HlsCoord.cancel`)
/// and the per-track [`AssetStore`] used by reader paths and by every
/// variant's `dispatch` closures.
pub(crate) struct HlsCoordEnv {
    pub(crate) cancel: CancellationToken,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
}

pub(crate) struct HlsCoord {
    pub(crate) cancel: CancellationToken,
    pub(crate) timeline: Timeline,
    pub(crate) abr: AbrHandle,
    pub(crate) variants: Arc<Vec<Arc<HlsVariant>>>,
    pub(crate) active_variant: Arc<AtomicUsize>,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
    /// Archive of variants that previously held the reader, ordered by
    /// activation time (most-recent last). Each entry retains its own
    /// `byte_shift` / `served_from..served_until` window so cross-variant
    /// reads (e.g. Phase 2 box scan after an Auto switch) can resolve a
    /// byte to the variant that actually streamed it.
    history: RwLock<Vec<Arc<HlsVariant>>>,
}

impl HlsCoord {
    pub(crate) fn new(
        env: HlsCoordEnv,
        timeline: Timeline,
        abr: AbrHandle,
        variants: Arc<Vec<Arc<HlsVariant>>>,
    ) -> Self {
        let initial = abr.current_variant_index().unwrap_or(0);
        let active_variant = Arc::new(AtomicUsize::new(initial));
        Self {
            cancel: env.cancel,
            timeline,
            abr,
            variants,
            active_variant,
            asset_store: env.asset_store,
            history: RwLock::new(Vec::new()),
        }
    }

    pub(crate) fn active(&self) -> Option<&Arc<HlsVariant>> {
        let idx = self.active_variant.load(Ordering::Acquire);
        self.variants.get(idx)
    }

    pub(crate) fn position(&self) -> u64 {
        self.active().map_or(0, |v| v.get_position())
    }

    pub(crate) fn advance(&self, n: u64) {
        if let Some(v) = self.active() {
            v.advance(n);
        }
    }

    pub(crate) fn set_position(&self, pos: u64) {
        if let Some(v) = self.active() {
            v.set_position(pos);
        }
    }

    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        if let Some(active) = self.active()
            && let Some(r) = active.find_at_offset(byte_offset)
        {
            return Some(r);
        }
        self.history
            .lock_sync_read()
            .iter()
            .rev()
            .find_map(|v| v.find_at_offset(byte_offset))
    }

    /// Locate the variant that serves `byte_offset` in the combined byte
    /// stream. Mirrors [`Self::find_at_offset`] but also returns the
    /// variant itself so callers can read its init / segment resource.
    pub(crate) fn resolve_variant(
        &self,
        byte_offset: u64,
    ) -> Option<(Arc<HlsVariant>, u32, u64, u64)> {
        if let Some(active) = self.active()
            && let Some((idx, off, size)) = active.find_at_offset(byte_offset)
        {
            return Some((Arc::clone(active), idx, off, size));
        }
        self.history.lock_sync_read().iter().rev().find_map(|v| {
            v.find_at_offset(byte_offset)
                .map(|(idx, off, size)| (Arc::clone(v), idx, off, size))
        })
    }

    /// Header (init / segment-0) virtual byte range of the most recent
    /// archived variant that still has one. After an ABR commit the
    /// active variant's `served_from > 0` and its own init lives in
    /// natural space only; this fallback hands the audio FSM the init
    /// range of the variant the reader was on just before the switch
    /// (same-codec ABR can reuse its moov for the recreated decoder).
    pub(crate) fn last_header_byte_range_in_history(&self) -> Option<Range<u64>> {
        self.history
            .lock_sync_read()
            .iter()
            .rev()
            .find_map(|v| v.header_byte_range())
    }

    /// Init prefix descriptor (key + size) for the byte at `byte_offset`,
    /// resolved against active + historical variants. Returns `None` when
    /// the byte falls outside any variant's *virtually addressable* init
    /// range — the active variant's init only counts when its
    /// `served_from() == 0` (post-commit, its init is orphaned in
    /// natural space and the historical predecessor is the right owner
    /// of that virtual prefix).
    pub(crate) fn init_descriptor_at(&self, byte_offset: u64) -> Option<(ResourceKey, Range<u64>)> {
        if let Some(active) = self.active()
            && active.served_from() == 0
        {
            let range = active.init_byte_range();
            if !range.is_empty()
                && range.contains(&byte_offset)
                && let Some(key) = active.init_resource()
            {
                return Some((key, range));
            }
        }
        self.history.lock_sync_read().iter().rev().find_map(|v| {
            if v.served_from() > 0 {
                return None;
            }
            let range = v.init_byte_range();
            if range.is_empty() || !range.contains(&byte_offset) {
                return None;
            }
            Some((v.init_resource()?, range))
        })
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        let active_total = self.active().map_or(0, |v| v.total_bytes());
        let history_max = self
            .history
            .lock_sync_read()
            .iter()
            .map(|v| v.total_bytes())
            .max()
            .unwrap_or(0);
        active_total.max(history_max)
    }

    pub(crate) fn variant_index(&self) -> usize {
        self.active_variant.load(Ordering::Acquire)
    }

    /// Number of consecutive `Loaded` segments from the start of the
    /// active variant — the ABR controller's "download head" signal.
    pub(crate) fn download_head(&self) -> u32 {
        self.active().map_or(0, |v| v.download_head())
    }

    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    /// Mirror `abr.lock()` state to `timeline.is_seek_pending()`.
    pub(crate) fn sync_abr_lock(&self) {
        let pending = self.timeline.is_seek_pending();
        let locked = self.abr.is_locked();
        if pending && !locked {
            self.abr.lock();
        } else if !pending && locked {
            self.abr.unlock();
        }
    }

    /// Commit any ABR pending decision at the reader's segment boundary.
    /// On a real switch:
    /// 1. caps the outgoing variant's `served_until` to `from_seg` and
    ///    archives it to `history` — future cross-variant reads (Phase 2
    ///    box scan, etc.) resolve bytes below the boundary through the
    ///    archived variant's resources;
    /// 2. positions the incoming variant at `from_seg` with `byte_shift`
    ///    pinned to the **segment boundary** (`v_old.segment_byte_offset(
    ///    from_seg)`) so fMP4 box addresses across the join stay
    ///    aligned; reader's cursor stays continuous via `reader_pos`
    ///    (which may sit a few bytes past the boundary when the reader
    ///    had partially crossed into the outgoing variant's `from_seg`
    ///    before the commit fired);
    /// 3. rebuilds the incoming variant's queue, atomically flips
    ///    `active_variant`, and emits `notify_commit`.
    ///
    /// Returns `true` when a switch landed.
    pub(crate) fn commit_variant_switch(&self, ctx: &PlanCtx, from_seg: u32) -> bool {
        let Some(decision) = self.abr.commit_pending(Instant::now()) else {
            return false;
        };
        if !decision.did_change {
            return false;
        }
        let current_before = self.variant_index();
        let new_v = decision.target_variant_index;
        let Some(v_new) = self.variants.get(new_v) else {
            return false;
        };
        let reader_pos = self.position();
        let v_old_opt = self.variants.get(current_before);
        let seg_boundary = v_old_opt
            .and_then(|v| v.segment_byte_offset(from_seg))
            .unwrap_or(reader_pos);
        if let Some(v_old) = v_old_opt {
            v_old.cancel();
            v_old.set_served_until(from_seg);
            self.history.lock_sync_write().push(Arc::clone(v_old));
        }
        v_new.activate_at_segment_with_shift(ctx, from_seg, seg_boundary, reader_pos);
        self.active_variant.store(new_v, Ordering::Release);
        let reader_pt = self.timeline.committed_position();
        self.abr
            .notify_commit(decision, current_before, reader_pt, Instant::now());
        true
    }

    /// Process one evicted resource key. Marks the lost segment `Missing`
    /// on every variant that owned it. When the active variant is among
    /// them, fires a full `rebuild` from the reader's current segment so
    /// the queue is refilled with the now-Missing slot reincluded.
    /// Non-active variants stay in a relaxed state — their next
    /// activation (ABR flip) calls `rebuild` and picks up the Missing
    /// entries then.
    pub(crate) fn broadcast_eviction(&self, ctx: &PlanCtx, key: &ResourceKey, seg_at_reader: u32) {
        let active_idx = self.variant_index();
        let mut active_lost = false;
        for (v_idx, v) in self.variants.iter().enumerate() {
            if v.on_evict(key).is_some() && v_idx == active_idx {
                active_lost = true;
            }
        }
        if active_lost && let Some(active) = self.active() {
            active.rebuild(ctx, seg_at_reader);
        }
    }
}

/// `SegmentLayout` delegates to whichever variant is currently active —
/// `HlsCoord` already owns the variants and the active index, so we
/// implement the trait here instead of a separate view wrapper.
impl SegmentLayout for HlsCoord {
    fn init_segment_range(&self) -> Range<u64> {
        if let Some(active) = self.active() {
            let range = active.init_byte_range();
            if !range.is_empty() {
                return range;
            }
        }
        // Active's init is orphaned post-ABR commit (served_from > 0). The
        // demuxer reads via the byte-virtual source; v_prev still serves
        // [0..init_size] virtually, and same-codec ABR keeps the same moov
        // there. Mirror `format_change_segment_range`'s history fallback.
        self.last_header_byte_range_in_history().unwrap_or(0..0)
    }

    fn segment_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        self.active()?.descriptor_after_byte(byte)
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.active()?.descriptor_at_time(t)
    }

    fn segment_count(&self) -> Option<u32> {
        Some(self.active()?.num_segments())
    }

    fn len(&self) -> Option<u64> {
        Some(self.active()?.total_bytes())
    }
}
