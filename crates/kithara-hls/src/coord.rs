#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use delegate::delegate;
use kithara_abr::AbrHandle;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::{
    time::{Duration, Instant},
    tokio::sync::Notify,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    MediaInfo, PendingReason, ReadOutcome, SegmentDescriptor, SegmentLayout, SourcePhase,
    SourceSeekAnchor, StreamResult, Timeline,
};
use tokio_util::sync::CancellationToken;

use crate::{
    playlist::PlaylistState,
    variant::{HlsVariant, PlanCtx},
};

/// Infrastructure handles shared with every [`HlsCoord`]:
/// the parent cancel token (cancel hierarchy owner of `HlsCoord.cancel`)
/// and the per-track [`AssetStore`] used by reader paths and by every
/// variant's `dispatch` closures.
pub(crate) struct HlsCoordEnv {
    pub(crate) cancel: CancellationToken,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
}

/// Thin router over a fixed `Vec<Arc<HlsVariant>>`. Every `Source`-side
/// reader op is delegated to `self.active()` (the variant pinned by
/// `active_variant`). The coord owns only what is genuinely
/// cross-variant: the active-index atomic, the ABR handle, the
/// cancel-hierarchy parent, the variant-change fence, and the per-track
/// playlist/asset/timeline references it hands to variants and to peer
/// `PlanCtx`-builders.
pub(crate) struct HlsCoord {
    pub(crate) cancel: CancellationToken,
    pub(crate) timeline: Timeline,
    pub(crate) abr: AbrHandle,
    pub(crate) variants: Arc<Vec<Arc<HlsVariant>>>,
    pub(crate) active_variant: Arc<AtomicUsize>,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
    playlist_state: Arc<PlaylistState>,
    /// Reader→peer wake handle, installed once when the owning `HlsPeer`
    /// binds. Forwarded to every variant via [`Self::set_peer_wake`] so
    /// `wait_range` / `seek_time_anchor` running inside a variant can
    /// resume `HlsPeer::poll_next` directly.
    peer_wake: OnceLock<Arc<Notify>>,
    /// Monotonic counter bumped by [`Self::commit_variant_switch`] on
    /// cross-codec switches. `read_at` / `wait_range` compare against
    /// [`Self::fence_at`] and short-circuit with `Pending(VariantChange)`
    /// / `Interrupted` until the audio FSM acks via
    /// [`Self::clear_variant_fence`]. Same-codec switches do not bump
    /// it — smooth ABR (FLAC@hi → FLAC@lo) keeps reading without a
    /// fence.
    variant_generation: AtomicU64,
    /// Last generation acknowledged by the reader. When `<
    /// variant_generation` the read gate is closed; when equal the gate
    /// is open. [`Self::clear_variant_fence`] copies the current
    /// generation here.
    fence_at: AtomicU64,
}

impl HlsCoord {
    pub(crate) fn new(
        env: HlsCoordEnv,
        timeline: Timeline,
        abr: AbrHandle,
        variants: Arc<Vec<Arc<HlsVariant>>>,
        playlist_state: Arc<PlaylistState>,
    ) -> Self {
        assert!(
            !variants.is_empty(),
            "HlsCoord constructed without variants — caller must supply at least one"
        );
        let initial = abr.current_variant_index().unwrap_or(0);
        let active_variant = Arc::new(AtomicUsize::new(initial));
        Self {
            cancel: env.cancel,
            timeline,
            abr,
            variants,
            active_variant,
            asset_store: env.asset_store,
            playlist_state,
            peer_wake: OnceLock::new(),
            variant_generation: AtomicU64::new(0),
            fence_at: AtomicU64::new(0),
        }
    }

    /// Install the wake handle that `HlsPeer` listens on. Called once by
    /// `HlsSource::set_hls_peer` after the peer is bound. Subsequent
    /// calls silently keep the first registration. Forwarded to every
    /// variant so reader-path waits inside a variant can wake the peer
    /// directly.
    pub(crate) fn set_peer_wake(&self, notify: &Arc<Notify>) {
        if self.peer_wake.set(Arc::clone(notify)).is_ok() {
            for v in self.variants.iter() {
                v.set_peer_wake(Arc::clone(notify));
            }
        }
    }

    fn wake_peer(&self) {
        if let Some(notify) = self.peer_wake.get() {
            notify.notify_one();
        }
    }

    pub(crate) fn active(&self) -> Option<&Arc<HlsVariant>> {
        let idx = self.active_variant.load(Ordering::Acquire);
        self.variants.get(idx)
    }

    /// Variants `Vec` is constructed non-empty (asserted in [`Self::new`])
    /// and `active_variant` is initialised from `abr.current_variant_index()`
    /// or 0; this lookup therefore always succeeds. Used by `delegate!`
    /// targets so trait methods don't need to thread `Option`s.
    fn active_required(&self) -> &Arc<HlsVariant> {
        self.active()
            .expect("HlsCoord constructed without variants — bug")
    }

    pub(crate) fn variant_index(&self) -> usize {
        self.active_variant.load(Ordering::Acquire)
    }

    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    delegate! {
        to self.active_required().as_ref() {
            #[call(get_position)]
            pub(crate) fn position(&self) -> u64;
            pub(crate) fn advance(&self, n: u64);
            pub(crate) fn set_position(&self, pos: u64);
            pub(crate) fn total_bytes(&self) -> u64;
            pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)>;
            pub(crate) fn download_head(&self) -> u32;
            pub(crate) fn format_change_segment_range(&self) -> Option<Range<u64>>;
            pub(crate) fn seek_time_anchor(
                &self,
                position: Duration,
            ) -> StreamResult<Option<SourceSeekAnchor>>;
        }
    }

    /// Active variant's media info. `HlsCoord` is constructed
    /// non-empty (asserted in [`Self::new`]) so this always succeeds —
    /// the `Source` trait's `Option<MediaInfo>` shape is restored at
    /// the [`HlsSource`](crate::source::HlsSource) façade.
    pub(crate) fn media_info(&self) -> MediaInfo {
        self.active_required().media_info()
    }

    /// Track-level phase. Master-cancel takes precedence (terminal
    /// `Cancelled`); otherwise the active variant's view of `range_ready`
    /// / `is_flushing` / `total_bytes` decides.
    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.cancel.is_cancelled() {
            return SourcePhase::Cancelled;
        }
        self.active_required().phase_at(range)
    }

    /// Total bytes are >0 — the value used by `Source::len` accessor.
    pub(crate) fn len(&self) -> Option<u64> {
        let total = self.total_bytes();
        (total > 0).then_some(total)
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

    /// Reset the active variant to a "fresh" single-variant layout on
    /// seek. Random seek may land far from the post-ABR-commit window,
    /// so collapse `byte_shift` / `served_from` / `served_until` back
    /// to the natural range — subsequent ABR commits at boundary will
    /// re-build the layering as usual.
    pub(crate) fn reset_for_seek(&self) {
        if let Some(active) = self.active() {
            active.reset_to_full_range();
        }
    }

    /// Commit any ABR pending decision at the reader's segment boundary.
    /// Returns `true` when a switch landed.
    ///
    /// Same-codec (FLAC@hi → FLAC@lo, AAC@hi → AAC@lo): smooth ABR via
    /// [`HlsVariant::activate_at_segment_with_shift`] — `byte_shift`
    /// pins `v_new`'s `from_seg` to the outgoing variant's segment
    /// boundary so fMP4 box addresses across the join stay aligned and
    /// the decoder reads forward without a pause.
    ///
    /// Cross-codec (FLAC ↔ AAC, fmp4 ↔ aac, …): hard reset on `v_new`
    /// via [`HlsVariant::reset_to_full_range`] (own native space, init
    /// at `[0, init_size)`), reader position seeded to 0, and
    /// `variant_generation` bumped — the next [`Self::read_at`] /
    /// [`Self::wait_range`] short-circuits with
    /// `Pending(VariantChange)` / `Interrupted` until the audio FSM
    /// recreates the decoder and acks via [`Self::clear_variant_fence`].
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
        let v_old = self.variants.get(current_before);
        let codec_changed = self.cross_codec(current_before, new_v);
        if codec_changed {
            if let Some(v_old) = v_old {
                v_old.cancel();
            }
            v_new.reset_to_full_range();
            v_new.set_position(0);
            // Close the fence BEFORE flipping `active_variant`. Reversed
            // order would leave a window where a reader observes the new
            // active but `variant_change_pending()` is still false —
            // pulling stale bytes from `v_new.position = 0` and an
            // uninitialised init range. The audio FSM acks the switch
            // via `clear_variant_fence` after recreating the decoder.
            self.variant_generation.fetch_add(1, Ordering::Release);
            self.active_variant.store(new_v, Ordering::Release);
            // Seed `v_new`'s fetch queue: cross-codec landed without a
            // segment boundary, so unlike `activate_at_segment_with_shift`
            // (same-codec branch) we never rebuilt the queue. The peer
            // would otherwise spin on `no commands seg=1` while the audio
            // FSM waits for an init segment that nobody dispatches.
            v_new.rebuild(ctx, 0);
        } else {
            // `from_seg` is the segment the reader has just entered;
            // pin v_new to the *next* segment so v_old keeps serving
            // the in-progress one (avoids a duplicate
            // `SegmentReadStart(v_old, from_seg)` + `SegmentReadStart(
            // v_new, from_seg)` pair when the reader cursor lingers
            // across the boundary).
            let switch_at = from_seg.saturating_add(1).min(v_new.num_segments());
            let reader_pos = self.position();
            let seg_boundary = v_old
                .and_then(|v| v.segment_byte_offset(switch_at))
                .unwrap_or(reader_pos);
            if let Some(v_old) = v_old {
                v_old.cancel();
                v_old.set_served_until(switch_at);
            }
            v_new.activate_at_segment_with_shift(ctx, switch_at, seg_boundary, reader_pos);
            self.active_variant.store(new_v, Ordering::Release);
        }
        let reader_pt = self.timeline.committed_position();
        self.abr
            .notify_commit(decision, current_before, reader_pt, Instant::now());
        true
    }

    fn cross_codec(&self, old: usize, new: usize) -> bool {
        let p = &self.playlist_state;
        p.variant_codec(old) != p.variant_codec(new)
            || p.variant_container(old) != p.variant_container(new)
    }

    fn variant_change_pending(&self) -> bool {
        self.variant_generation.load(Ordering::Acquire) > self.fence_at.load(Ordering::Acquire)
    }

    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        if self.cancel.is_cancelled() {
            return Err(kithara_stream::StreamError::Source(
                crate::HlsError::Cancelled.into(),
            ));
        }
        if self.variant_change_pending() {
            return Ok(ReadOutcome::Pending(PendingReason::VariantChange));
        }
        self.active_required().read_at(offset, buf)
    }

    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        if self.cancel.is_cancelled() {
            return Err(kithara_stream::StreamError::Source(
                crate::HlsError::Cancelled.into(),
            ));
        }
        if self.variant_change_pending() {
            return Ok(WaitOutcome::Interrupted);
        }
        self.active_required().wait_range(range, timeout)
    }

    /// Wake the peer on receipt of a new seek epoch. The epoch value
    /// itself lives on `Timeline` — we just resume `poll_next`.
    pub(crate) fn set_seek_epoch(&self, _seek_epoch: u64) {
        self.wake_peer();
    }

    /// Notify the audio FSM that the cross-codec switch is acknowledged
    /// — opens the read gate by aligning `fence_at` to the current
    /// generation. Called from `HlsSource::clear_variant_fence` after
    /// the decoder has been recreated against the new variant.
    pub(crate) fn clear_variant_fence(&self) {
        self.fence_at.store(
            self.variant_generation.load(Ordering::Acquire),
            Ordering::Release,
        );
    }

    /// External signal that the reader is blocked — wake the peer so
    /// the next `poll_next` runs immediately.
    pub(crate) fn notify_waiting(&self) {
        self.wake_peer();
    }

    /// Process one evicted resource key. Marks the lost segment
    /// `Missing` on every variant that owned it. When the active
    /// variant is among them, fires a full `rebuild` from the reader's
    /// current segment so the queue is refilled with the now-Missing
    /// slot reincluded. Non-active variants stay relaxed — their next
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
        self.active().map(|v| v.init_byte_range()).unwrap_or(0..0)
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
