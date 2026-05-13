#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara_abr::AbrHandle;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::time::Instant;
use kithara_stream::Timeline;
use tokio_util::sync::CancellationToken;

use crate::variant::{HlsVariant, PlanCtx, segment_view::HlsSegmentView};

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
    pub(crate) variants: Arc<Vec<HlsVariant>>,
    pub(crate) active_variant: Arc<AtomicUsize>,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
    segment_view: Arc<HlsSegmentView>,
}

impl HlsCoord {
    pub(crate) fn new(
        env: HlsCoordEnv,
        timeline: Timeline,
        abr: AbrHandle,
        variants: Arc<Vec<HlsVariant>>,
    ) -> Self {
        let initial = abr.current_variant_index().unwrap_or(0);
        let active_variant = Arc::new(AtomicUsize::new(initial));
        let segment_view = Arc::new(HlsSegmentView::new(
            Arc::clone(&variants),
            Arc::clone(&active_variant),
        ));
        Self {
            cancel: env.cancel,
            timeline,
            abr,
            variants,
            active_variant,
            asset_store: env.asset_store,
            segment_view,
        }
    }

    pub(crate) fn active(&self) -> Option<&HlsVariant> {
        let idx = self.active_variant.load(Ordering::Acquire);
        self.variants.get(idx)
    }

    pub(crate) fn position(&self) -> u64 {
        self.active().map_or(0, HlsVariant::get_position)
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
        self.active()?.find_at_offset(byte_offset)
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.active().map_or(0, HlsVariant::total_bytes)
    }

    pub(crate) fn variant_index(&self) -> usize {
        self.active_variant.load(Ordering::Acquire)
    }

    /// Number of consecutive `Loaded` segments from the start of the
    /// active variant — the ABR controller's "download head" signal.
    pub(crate) fn download_head(&self) -> u32 {
        self.active().map_or(0, HlsVariant::download_head)
    }

    pub(crate) fn segment_view(&self) -> Arc<HlsSegmentView> {
        Arc::clone(&self.segment_view)
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
    /// On a real switch: cancels the outgoing variant, positions the
    /// incoming variant at `from_seg`, atomically flips `active_variant`,
    /// rebuilds the new variant's queue, and emits `notify_commit`.
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
        if let Some(v_old) = self.variants.get(current_before) {
            v_old.cancel();
        }
        v_new.activate_at_segment(ctx, from_seg);
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
