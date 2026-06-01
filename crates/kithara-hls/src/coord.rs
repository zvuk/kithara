#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use delegate::delegate;
use kithara_abr::AbrHandle;
use kithara_assets::{AssetScope, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::AbrReason;
use kithara_platform::{
    CancellationToken,
    time::{Duration, Instant},
    tokio::sync::Notify,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    ContainerFormat, MediaInfo, PendingReason, ReadOutcome, SegmentDescriptor, SegmentLayout,
    SourcePhase, SourceSeekAnchor, StreamResult, Timeline,
};
use kithara_test_utils::kithara;
use tracing::info;

use crate::{
    playlist::{PlaylistAccess, PlaylistState},
    variant::{HlsVariant, PlanCtx},
};

/// Infrastructure handles shared with every [`HlsCoord`]:
/// the parent cancel token (cancel hierarchy owner of `HlsCoord.cancel`)
/// and the per-track [`AssetStore`] used by reader paths and by every
/// variant's `dispatch` closures.
pub(crate) struct HlsCoordEnv {
    pub(crate) scope: AssetScope<DecryptContext>,
    pub(crate) cancel: CancellationToken,
    pub(crate) headers: Option<kithara_net::Headers>,
}

/// Thin router over a fixed `Vec<Arc<HlsVariant>>`. Every `Source`-side
/// reader op is delegated to `self.active()` (the variant whose index
/// `AbrState::current_variant_index` resolves to). The coord owns only
/// what is genuinely cross-variant: the ABR handle (single source of
/// truth for variant index), the cancel-hierarchy parent, the
/// variant-change fence, and the per-track playlist/asset/timeline
/// references it hands to variants and to peer `PlanCtx`-builders.
pub(crate) struct HlsCoord {
    pub(crate) abr: AbrHandle,
    pub(crate) scope: AssetScope<DecryptContext>,
    pub(crate) variants: Arc<Vec<Arc<HlsVariant>>>,
    pub(crate) cancel: CancellationToken,
    pub(crate) headers: Option<kithara_net::Headers>,
    pub(crate) timeline: Timeline,
    playlist_state: Arc<PlaylistState>,
    /// Last generation acknowledged by the reader. When `<
    /// variant_generation` the read gate is closed; when equal the gate
    /// is open. [`Self::clear_variant_fence`] copies the current
    /// generation here.
    fence_at: AtomicU64,
    /// Monotonic counter bumped by [`Self::commit_variant_switch`] on
    /// cross-codec switches. `read_at` / `wait_range` compare against
    /// [`Self::fence_at`] and short-circuit with `Pending(VariantChange)`
    /// / `Interrupted` until the audio FSM acks via
    /// [`Self::clear_variant_fence`]. Same-codec switches do not bump
    /// it — smooth ABR (FLAC@hi → FLAC@lo) keeps reading without a
    /// fence.
    variant_generation: AtomicU64,
    /// Reader→peer wake handle, installed once when the owning `HlsPeer`
    /// binds. Forwarded to every variant via [`Self::set_peer_wake`] so
    /// `wait_range` / `seek_time_anchor` running inside a variant can
    /// resume `HlsPeer::poll_next` directly.
    peer_wake: OnceLock<Arc<Notify>>,
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
        assert!(
            abr.current_variant_index().is_some(),
            "HlsCoord requires an AbrHandle with state — HlsPeer must construct AbrState"
        );
        Self {
            timeline,
            abr,
            variants,
            playlist_state,
            cancel: env.cancel,
            scope: env.scope,
            headers: env.headers,
            peer_wake: OnceLock::new(),
            variant_generation: AtomicU64::new(0),
            fence_at: AtomicU64::new(0),
        }
    }

    pub(crate) fn active(&self) -> Option<&Arc<HlsVariant>> {
        self.variants.get(self.variant_index())
    }

    /// `AbrState` always returns a valid index (constructor asserts
    /// stateful handle), and `variants` is non-empty (asserted in
    /// [`Self::new`]). This lookup therefore always succeeds. Used by
    /// `delegate!` targets so trait methods don't need to thread
    /// `Option`s.
    fn active_required(&self) -> &Arc<HlsVariant> {
        self.active()
            .expect("HlsCoord constructed without variants — bug")
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
        let active_lost = self
            .variants
            .iter()
            .enumerate()
            .fold(false, |acc, (v_idx, v)| {
                let hit = v.on_evict(key).is_some() && v_idx == active_idx;
                acc || hit
            });
        if active_lost && let Some(active) = self.active() {
            active.rebuild(ctx, seg_at_reader);
        }
    }

    /// Notify the audio FSM that the cross-codec switch is acknowledged
    /// — opens the read gate by aligning `fence_at` to the current
    /// generation. Called from `HlsSource::clear_variant_fence` after
    /// the decoder has been recreated against the new variant.
    pub(crate) fn clear_variant_fence(&self) {
        let current_gen = self.variant_generation.load(Ordering::Acquire);
        let prev = self.fence_at.swap(current_gen, Ordering::AcqRel);
        if prev != current_gen {
            info!(
                fence_was = prev,
                fence_now = current_gen,
                "HlsCoord: clear_variant_fence"
            );
        }
    }

    /// Commit any ABR pending decision at the reader's segment boundary.
    /// Returns `true` when a switch landed.
    ///
    /// Two branches, selected by the new variant's container:
    ///
    /// - **Byte-continuity containers** (raw PCM in RIFF — `WAV`): the
    ///   decoder cannot be recreated mid-track (must read the header
    ///   at byte 0 and then consume PCM sequentially). Activate
    ///   `v_new` at the boundary segment with `byte_shift` so the
    ///   existing decoder keeps reading aligned bytes from the new
    ///   variant. No fence, no recreate.
    /// - **Structured containers** (fMP4, MPEG-TS, FLAC, …): hard
    ///   reset on `v_new` via [`HlsVariant::reset_to_full_range`],
    ///   reader position seeded to the segment covering the current
    ///   timeline position, and `variant_generation` bumped — the
    ///   next [`Self::read_at`] / [`Self::wait_range`]
    ///   short-circuits with `Pending(VariantChange)` /
    ///   `Interrupted` until the audio FSM recreates the decoder and
    ///   acks via [`Self::clear_variant_fence`].
    pub(crate) fn commit_variant_switch(&self, ctx: &PlanCtx, from_seg: u32) -> bool {
        let current_before = self.variant_index();
        let Some(decision) = self.abr.peek_pending_decision() else {
            return false;
        };
        let new_v = decision.target().get();
        let Some(v_new) = self.variants.get(new_v) else {
            return false;
        };
        let v_old = self.variants.get(current_before);
        let reader_pos_at_entry = self.position();
        let needs_byte_continuity = matches!(
            self.playlist_state.variant_container(new_v),
            Some(ContainerFormat::Wav)
        );
        info!(
            from_variant = current_before,
            to_variant = new_v,
            from_seg,
            reader_pos = reader_pos_at_entry,
            needs_byte_continuity,
            reason = ?decision.reason(),
            "HlsCoord: commit_variant_switch"
        );
        if needs_byte_continuity {
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
            self.abr.apply_decision(&decision, Instant::now());
        } else {
            let old_codec = v_old.and_then(|_| self.playlist_state.variant_codec(current_before));
            let new_codec = self.playlist_state.variant_codec(new_v);
            let is_cross_codec = match (old_codec, new_codec) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            };
            if let Some(v_old) = v_old {
                v_old.cancel();
            }
            v_new.reset_to_full_range();
            if is_cross_codec {
                v_new.invalidate_init();
            }
            let target_time = self
                .timeline
                .seek_target()
                .unwrap_or_else(|| self.timeline.committed_position());
            let target_seg: u32 = self
                .playlist_state
                .find_seek_point_for_time(new_v, target_time)
                .and_then(|(seg, _, _)| u32::try_from(seg).ok())
                .unwrap_or(0);
            let target_byte = v_new.segment_byte_offset_natural(target_seg).unwrap_or(0);
            v_new.set_position(target_byte);
            self.variant_generation.fetch_add(1, Ordering::Release);
            self.abr.apply_decision(&decision, Instant::now());
            v_new.rebuild_with_decoder_probe(ctx, target_seg);
        }
        let reader_pt = self.timeline.committed_position();
        self.abr
            .notify_commit(decision, current_before, reader_pt, Instant::now());
        true
    }

    /// Break the urgent-down-switch / blocked-reader deadlock: when the
    /// active (slow) variant cannot deliver the next segment the reader
    /// needs, an Auto-mode commit would otherwise wait for a boundary
    /// cross that the undelivered segment prevents. Return the segment to
    /// commit at (`download_head - 1`, so `commit_variant_switch`'s
    /// `from_seg + 1` lands `switch_at = download_head`) when a proactive
    /// rescue is both warranted and continuity-safe; otherwise `None`.
    ///
    /// Guards (all required):
    /// - a pending decision exists and its reason is
    ///   [`AbrReason::UrgentDownSwitch`] — only the rescue path commits
    ///   early; opportunistic up/down-switches keep boundary-cross
    ///   gating so `v_new` is not pinned prematurely;
    /// - the target is a WAV byte-continuity variant — the structured
    ///   recreate path reseeds by time and is not subject to this
    ///   circular dependency;
    /// - `download_head` is strictly ahead of the reader's current
    ///   segment (`reader_seg`). This keeps the switch on a clean
    ///   segment boundary the reader has not begun consuming: the
    ///   reader finishes its fully-loaded current segment on `v_old`,
    ///   `v_new` takes over at `download_head`. When
    ///   `download_head == reader_seg` the reader is mid an undelivered
    ///   segment, so handing it to `v_new` would be a mid-segment
    ///   cross-bitrate switch (sample shift) — never rescue there;
    /// - `download_head < num_segments`, i.e. `v_old` genuinely has
    ///   un-downloaded tail (otherwise there is nothing to rescue from).
    pub(crate) fn urgent_rescue_boundary(&self, reader_seg: u32) -> Option<u32> {
        let decision = self.abr.peek_pending_decision()?;
        if decision.reason() != AbrReason::UrgentDownSwitch {
            return None;
        }
        if !matches!(
            self.playlist_state
                .variant_container(decision.target().get()),
            Some(ContainerFormat::Wav)
        ) {
            return None;
        }
        let head = self.download_head();
        let active = self.active()?;
        if head >= active.num_segments() || head <= reader_seg {
            return None;
        }
        Some(head.saturating_sub(1))
    }

    /// Cross-variant segment lookup. Mirrors [`Self::variant_serving`]'s
    /// priority: active first, then shrunk `v_old`s. Returns `None` if no
    /// engaged variant claims the offset.
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        let active = self.active_required();
        if let Some(found) = active.find_at_offset(byte_offset) {
            return Some(found);
        }
        for v in self.variants.iter() {
            if Arc::ptr_eq(v, active) {
                continue;
            }
            let shrunk = v.is_shrunk();
            if !shrunk {
                continue;
            }
            if let Some(found) = v.find_at_offset(byte_offset) {
                return Some(found);
            }
        }
        None
    }

    /// Public-API mirror of [`Self::variant_change_pending`] used by the
    /// audio decode loop to bail out of an `Ok(Pending(_))` spin when
    /// the underlying `VariantChangeError` was absorbed by the demuxer.
    pub(crate) fn has_variant_change_pending(&self) -> bool {
        self.variant_change_pending()
    }

    /// Total bytes are >0 — the value used by `Source::len` accessor.
    pub(crate) fn len(&self) -> Option<u64> {
        let total = self.total_bytes();
        (total > 0).then_some(total)
    }

    /// Active variant's media info. `HlsCoord` is constructed
    /// non-empty (asserted in [`Self::new`]) so this always succeeds —
    /// the `Source` trait's `Option<MediaInfo>` shape is restored at
    /// the [`HlsSource`](crate::source::HlsSource) façade.
    pub(crate) fn media_info(&self) -> MediaInfo {
        self.active_required().media_info()
    }

    /// External signal that the reader is blocked — wake the peer so
    /// the next `poll_next` runs immediately.
    pub(crate) fn notify_waiting(&self) {
        self.wake_peer();
    }

    /// Track-level phase. Master-cancel takes precedence (terminal
    /// `Cancelled`); otherwise the variant that currently serves
    /// `range.start` decides — mid-buffer boundary cross resolves to
    /// the right `range_ready` / `is_flushing` / `total_bytes` view.
    #[kithara::rtsan_allow_blocking]
    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.cancel.is_cancelled() {
            return SourcePhase::Cancelled;
        }
        self.variant_serving(range.start).phase_at(range)
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
        self.variant_serving(offset).read_at(offset, buf)
    }

    /// Reset the active variant to a "fresh" single-variant layout on
    /// seek. Random seek may land far from the post-ABR-commit window,
    /// so collapse `byte_shift` / `served_from` / `served_until` back
    /// to the natural range: subsequent ABR commits at boundary will
    /// re-build the layering as usual.
    ///
    /// Also drops any unobserved throughput-driven boundary-commit
    /// decision: a pending up-switch chosen against pre-seek throughput
    /// is stale once the reader jumps and would otherwise commit on
    /// the first boundary after the seek lands, forcing decoder recreate
    /// before the new-variant cache is warm (a `HangDetector` trip).
    pub(crate) fn reset_for_seek(&self) {
        if let Some(active) = self.active() {
            active.reset_to_full_range();
        }
        self.abr.invalidate_pending();
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

    /// Wake the peer on receipt of a new seek epoch. The epoch value
    /// itself lives on `Timeline` — we just resume `poll_next`.
    pub(crate) fn set_seek_epoch(&self, _seek_epoch: u64) {
        self.wake_peer();
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

    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn variant_change_pending(&self) -> bool {
        self.variant_generation.load(Ordering::Acquire) > self.fence_at.load(Ordering::Acquire)
    }

    /// Single source of truth: the variant index lives in
    /// [`AbrState::current_variant`]. The previous duplicate
    /// `HlsCoord::active_variant` was removed — `apply_decision`
    /// publishes the switch after `v_new` is fully prepared, so a
    /// reader observing `current_variant := new` is guaranteed to see
    /// `v_new` ready.
    pub(crate) fn variant_index(&self) -> usize {
        self.abr
            .current_variant_index()
            .expect("HlsCoord requires AbrHandle with state — checked in new()")
    }

    /// Find the variant whose served range covers `offset`. Priority:
    ///
    /// 1. The ABR-active variant — the normal steady-state hit.
    /// 2. Any non-active variant whose served range has been *shrunk*
    ///    from its default span by a prior ABR commit (i.e.
    ///    `served_from > 0` or `served_until < num_segments`). These
    ///    are `v_old`s that still serve their pre-switch byte range so
    ///    a reader crossing the boundary mid-buffer hits the right
    ///    payload.
    ///
    /// Idle variants with default served bounds are deliberately
    /// excluded: their layout overlaps the active range but their
    /// resources were never fetched, so routing to them would return
    /// `NotFound` / `Pending(Retry)`.
    pub(crate) fn variant_serving(&self, offset: u64) -> &Arc<HlsVariant> {
        let active = self.active_required();
        if active.init_descriptor_at(offset).is_some() || active.find_at_offset(offset).is_some() {
            return active;
        }
        for v in self.variants.iter() {
            if Arc::ptr_eq(v, active) {
                continue;
            }
            let shrunk = v.is_shrunk();
            if !shrunk {
                continue;
            }
            if v.init_descriptor_at(offset).is_some() || v.find_at_offset(offset).is_some() {
                return v;
            }
        }
        active
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
        self.variant_serving(range.start).wait_range(range, timeout)
    }

    fn wake_peer(&self) {
        if let Some(notify) = self.peer_wake.get() {
            notify.notify_one();
        }
    }

    delegate! {
        to self.active_required().as_ref() {
            #[call(get_position)]
            pub(crate) fn position(&self) -> u64;
            pub(crate) fn advance(&self, n: u64);
            pub(crate) fn set_position(&self, pos: u64);
            pub(crate) fn total_bytes(&self) -> u64;
            pub(crate) fn download_head(&self) -> u32;
            pub(crate) fn format_change_segment_range(&self) -> StreamResult<Range<u64>>;
            pub(crate) fn seek_time_anchor(
                &self,
                position: Duration,
            ) -> StreamResult<Option<SourceSeekAnchor>>;
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

    fn len(&self) -> Option<u64> {
        Some(self.active()?.total_bytes())
    }

    fn segment_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        self.active()?.descriptor_after_byte(byte)
    }

    fn segment_at_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        self.variant_serving(byte).descriptor_at_byte(byte)
    }

    fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
        self.active()?.descriptor(segment_index as usize)
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.active()?.descriptor_at_time(t)
    }

    fn segment_count(&self) -> Option<u32> {
        Some(self.active()?.num_segments())
    }
}
