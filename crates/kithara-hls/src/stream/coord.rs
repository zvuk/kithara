#![forbid(unsafe_code)]

use std::{ops::Range, sync::atomic::AtomicU64};

use delegate::delegate;
use kithara_abr::{AbrHandle, AbrPublisher};
use kithara_assets::{AssetScope, ResourceKey};
use kithara_events::{DeferredBus, HlsEvent};
use kithara_platform::{
    CancelToken,
    sync::{Arc, WaitGate},
    time::Duration,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, ByteMap, DeferredWake, MediaInfo, PlayheadRead, PlayheadState, PlayheadWrite,
    ReadOutcome, ReaderProfile, SeekControl, SeekObserve, SeekPrepare, SeekState,
    SegmentDescriptor, SourceError, SourcePhase, SourceSeekAnchor, StreamError, StreamResult,
    VariantControl, VariantPromotion, VariantReaderPlan, VariantReaderTake, VariantTransition,
};
use kithara_test_utils::kithara;

use super::{session::HlsSession, transition::SessionSlots};
use crate::{
    signal::SizeSignal,
    variant::{HlsVariant, PlanCtx},
};

/// Watchdog timeout for the off-RT blocking `wait_range(_, None)`: must exceed
/// the `kithara-net` per-fetch total timeout so a stalled upstream is failed by
/// the network layer (the wait then returns a terminal `Err`) before this
/// deadlock watchdog fires. Mirrors `kithara-storage` `WAIT_HANG_TIMEOUT`. Only
/// a wait that never wakes after every signal site fired is a real deadlock.
const WAIT_HANG_TIMEOUT: Duration = Duration::from_secs(180);

/// Infrastructure handles shared with every [`HlsCoord`]:
/// the parent cancel token (cancel hierarchy owner of `HlsCoord.cancel`)
/// and the per-track [`AssetStore`] used by reader paths and by every
/// variant's `dispatch` closures.
pub(crate) struct HlsCoordEnv {
    pub(crate) emit: Arc<DeferredBus<HlsEvent>>,
    pub(crate) scope: AssetScope,
    pub(crate) cancel: CancelToken,
    pub(crate) headers: Option<kithara_net::Headers>,
    /// Unified reader-wake handle: the shared readiness gate paired with the
    /// late-bound audio-worker wake. Every transition that can flip a blocked
    /// reader's `wait_range` predicate (segment write/commit/fail, seek reset,
    /// cancel) signals the gate; the off-RT `wait_range(_, None)` parks on it
    /// instead of polling a wall-clock timer. The two downloader write/settle
    /// sites and the coord's seek preparation re-tick the audio worker via
    /// [`SizeSignal::fire`]. See `CONTEXT.md` "Event-driven read wait".
    pub(crate) signal: SizeSignal,
}

/// Coordinator over fixed variants and one authoritative active reader session.
/// The optional incoming session stays private until exact audio promotion.
pub(crate) struct HlsCoord {
    pub(crate) abr: AbrHandle,
    pub(crate) emit: Arc<DeferredBus<HlsEvent>>,
    pub(crate) variants: Arc<[Arc<HlsVariant>]>,
    pub(crate) scope: AssetScope,
    pub(crate) cancel: CancelToken,
    pub(crate) headers: Option<kithara_net::Headers>,
    pub(super) abr_publisher: AbrPublisher,
    /// One authoritative active session and at most one exact incoming session.
    pub(super) sessions: SessionSlots,
    /// Backing playhead state — the coord owns the `Arc` directly and
    /// vends narrow trait-object handles from it.
    playhead: Arc<PlayheadState>,
    /// Backing seek/activity state — the coord owns the `Arc` directly and
    /// vends narrow trait-object handles from it.
    seek: Arc<SeekState>,
    /// Narrow seek-observe handle — derived from `seek` at construction.
    /// Used by internal methods that only need epoch/target/pending reads.
    seek_obs: Arc<dyn SeekObserve>,
    /// Unified reader-wake handle for the off-RT blocking `wait_range(_, None)`.
    /// Shared with every variant's fetch closures (write/commit/fail
    /// [`SizeSignal::fire`] it) and fired by the coord on a seek reset.
    /// See [`HlsCoordEnv::signal`].
    signal: SizeSignal,
}

impl HlsCoord {
    /// Re-aim heartbeat for the off-RT blocking wait. The wait wakes immediately
    /// on any readiness signal (the fact of a write/commit/fence/seek) —
    /// event-driven. This interval bounds only the *quiet* case: if no signal
    /// arrives within it, the peer may be mis-aimed after a seek (it fetched,
    /// went idle, and the range the reader now wants is outside its prefetch
    /// window), so the wait yields `WaitBudgetExceeded` to let the off-RT reader
    /// re-assert the peer's aim (`notify_peer_wake`) and re-enter. It never polls
    /// for data — readiness is always learned from a signal, never from a timer.
    const READER_REAIM_INTERVAL: Duration = Duration::from_millis(25);

    pub(crate) fn new(
        env: HlsCoordEnv,
        playhead: Arc<PlayheadState>,
        seek: Arc<SeekState>,
        abr: AbrHandle,
        abr_publisher: AbrPublisher,
        variants: Arc<[Arc<HlsVariant>]>,
    ) -> Self {
        assert!(
            !variants.is_empty(),
            "HlsCoord constructed without variants — caller must supply at least one"
        );
        assert!(
            abr.current_variant_index().is_some(),
            "HlsCoord requires an AbrHandle with state — HlsPeer must construct AbrState"
        );
        let seek_obs = Arc::clone(&seek) as Arc<dyn SeekObserve>;
        let active_index = abr
            .current_variant_index()
            .expect("stateful ABR handle checked above");
        let active_variant = Arc::clone(
            variants
                .get(active_index)
                .expect("ABR current variant must exist in HLS variants"),
        );
        let active_session = Arc::new(HlsSession::active(
            env.cancel.child(),
            Arc::clone(&seek_obs),
            env.signal.clone(),
            active_index,
            Arc::clone(&active_variant),
            0,
        ));
        Self {
            playhead,
            seek,
            seek_obs,
            abr,
            abr_publisher,
            variants,
            cancel: env.cancel,
            scope: env.scope,
            headers: env.headers,
            emit: env.emit,
            sessions: SessionSlots::new(active_session),
            signal: env.signal,
        }
    }

    pub(crate) fn activity(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
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
        if active_lost {
            self.active().rebuild(ctx, seg_at_reader);
        }
    }

    pub(super) fn commit_if_seek_epoch<T, F>(&self, epoch: u64, commit: F) -> Option<T>
    where
        F: FnOnce() -> T,
    {
        self.seek.commit_if_epoch(epoch, commit)
    }

    /// Resolve the segment whose fetch queue owns the reader cursor.
    ///
    /// This is deliberately wider than [`Self::find_at_offset`]: a cursor in
    /// the active variant's init prefix is not inside any media segment, but it
    /// still demands the segment-0 decoder probe plan. Cross-variant media
    /// lookups keep the existing `find_at_offset` routing for shrunk outgoing
    /// variants.
    pub(crate) fn demand_segment_at_offset(&self, byte_offset: u64) -> Option<u32> {
        let active = self.active();
        active
            .demand_segment_at_offset(byte_offset)
            .or_else(|| self.find_at_offset(byte_offset).map(|(idx, _, _)| idx))
    }

    /// Cross-variant segment lookup. Mirrors [`Self::variant_serving`]'s
    /// priority: active first, then shrunk `v_old`s. Returns `None` if no
    /// engaged variant claims the offset.
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        let active = self.active();
        if let Some(found) = active.find_at_offset(byte_offset) {
            return Some(found);
        }
        for v in self.variants.iter() {
            if Arc::ptr_eq(v, &active) {
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

    /// Track-level phase. Master-cancel takes precedence (terminal
    /// `Cancelled`); otherwise the variant that currently serves
    /// `range.start` decides — mid-buffer boundary cross resolves to
    /// the right `range_ready` / `is_flushing` / `total_bytes` view.
    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.cancel.is_cancelled() {
            return SourcePhase::Cancelled;
        }
        self.variant_serving(range.start).phase_at(range)
    }

    pub(crate) fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    pub(crate) fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    /// Seek entry point. Collapses cross-variant byte-continuity layering,
    /// cancels the incoming exact session, and wakes a parked reader.
    ///
    /// Runs on the control thread before the seek epoch is minted: the cancel
    /// and the collapse both take a lock, so no reader may reach them.
    ///
    /// The expensive layout collapse ([`Self::reset_for_seek`]) runs only
    /// when the active variant's offset table is not already the canonical
    /// full-range geometry with every served size exact — a fully-resolved
    /// single-variant track repeats the identical table, so the O(N) rebuild
    /// is skipped. The ABR invalidation and the reader wake stay
    /// unconditional, so the seek's cancel/wake semantics are unchanged for
    /// every track (cross-variant, partial-download, or fully cached).
    ///
    /// A pending decision survives the seek: its incoming session is rebuilt in
    /// the new seek epoch, and publication remains impossible until that
    /// replacement is ready.
    pub(crate) fn prepare_for_seek(&self) {
        self.cancel_incoming_for_seek();
        if !self.active().layout_seek_invariant() {
            self.reset_for_seek();
        }
        // A seek repositioned the active variant: wake a reader parked on the
        // pre-seek range so it re-probes against the new position / flush gate.
        self.signal.fire();
    }

    /// Single wake-free readiness probe (the wake-free `HlsVariant::wait_range`
    /// behind the coord's fence/cancel short-circuits). Shared by the RT probe
    /// path and the off-RT blocking loop's per-iteration check.
    fn probe_range(
        &self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        if self.cancel.is_cancelled() {
            return Err(StreamError::Source(crate::HlsError::Cancelled.into()));
        }
        self.variant_serving(range.start).wait_range(range, timeout)
    }

    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        if self.cancel.is_cancelled() {
            return Err(StreamError::Source(crate::HlsError::Cancelled.into()));
        }
        self.variant_serving(offset).read_at(offset, buf)
    }

    /// Reconcile the ABR escape flag against the live stall geometry. Edge-
    /// detected against [`AbrHandle::is_escaping`]: a rising edge — the reader
    /// is newly parked at a clean boundary on a non-delivering segment — marks
    /// escape and returns `true`, signalling the caller to trigger a controller
    /// re-tick OUTSIDE the HLS state lock (the tick reads `peer.progress()`,
    /// which re-locks it, so a synchronous tick here would deadlock). A falling
    /// edge — the segment now delivers, or a switch was published — clears it.
    /// Idempotent in the steady state.
    pub(crate) fn reconcile_escape(&self, reader_seg: u32) -> bool {
        let stalled = self
            .active()
            .segment_stalled_at_boundary(reader_seg, self.position());
        let was = self.abr.is_escaping();
        if stalled && !was {
            self.abr.mark_escape();
            true
        } else {
            if !stalled && was {
                self.abr.clear_escape();
            }
            false
        }
    }

    pub(crate) fn seek_control(&self) -> Arc<dyn SeekControl> {
        Arc::clone(&self.seek) as Arc<dyn SeekControl>
    }

    pub(crate) fn seek_epoch_handle(&self) -> Arc<AtomicU64> {
        self.seek.seek_epoch_arc()
    }

    pub(crate) fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    pub(crate) fn selected_variant_for_seek(&self) -> usize {
        self.abr
            .selected_variant_for_seek()
            .expect("HlsCoord always owns a stateful ABR handle")
    }

    /// Install the peer's `reader_advanced` wake so the `on_slow` hook can
    /// re-poll the peer when an in-flight fetch stalls past `soft_timeout`.
    /// Called once by `HlsPeer::activate`.
    pub(crate) fn set_peer_wake(
        &self,
        deferred: Arc<DeferredWake>,
        direct: Arc<dyn kithara_stream::WorkerWake>,
    ) {
        self.signal.set_peer_wake(deferred, direct);
    }

    /// Install the audio worker's data-arrival wake (idempotent — only the
    /// first set sticks). Called by `HlsSource::set_worker_wake` once the
    /// worker exists; downloader fetch closures read it lock-free thereafter.
    pub(crate) fn set_worker_wake(&self, wake: Arc<dyn kithara_stream::WorkerWake>) {
        self.signal.set_worker_wake(wake);
    }

    /// The unified reader-wake handle, handed to [`PlanCtx`] so variant fetch
    /// closures can [`SizeSignal::fire`] on segment write/commit/fail.
    pub(crate) fn signal(&self) -> SizeSignal {
        self.signal.clone()
    }

    /// Mirror `abr.lock()` state to `seek_obs.is_pending()`.
    pub(crate) fn sync_abr_lock(&self) {
        let pending = self.seek_obs.is_pending();
        let locked = self.abr.is_locked();
        if pending && !locked {
            self.abr.lock();
        } else if !pending && locked {
            self.abr.unlock();
        }
    }

    /// Find the variant whose served range covers `offset`. Priority:
    ///
    /// 1. The active session's variant — the normal steady-state hit.
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
    pub(crate) fn variant_serving(&self, offset: u64) -> Arc<HlsVariant> {
        let active = self.active();
        if active.init_descriptor_at(offset).is_some() || active.find_at_offset(offset).is_some() {
            return active;
        }
        for v in self.variants.iter() {
            if Arc::ptr_eq(v, &active) {
                continue;
            }
            let shrunk = v.is_shrunk();
            if !shrunk {
                continue;
            }
            if v.init_descriptor_at(offset).is_some() || v.find_at_offset(offset).is_some() {
                return Arc::clone(v);
            }
        }
        active
    }

    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        match timeout {
            // RT / cooperative-yield probe path (`probe_read`): a single
            // wake-free probe, unchanged — never parks on the gate.
            Some(_) => self.probe_range(range, timeout),
            // Off-RT consumer (`Stream::read` / `prime_seek_range`): block on
            // the readiness gate until the range resolves, a segment fails, or
            // cancel fires. Event-driven — no wall-clock poll.
            None => Self::wait_range_blocking(&self.signal, &self.cancel, || {
                self.probe_range(range.clone(), Some(Duration::ZERO))
            }),
        }
    }

    /// Off-RT blocking wait: park on the readiness gate until [`probe_range`]
    /// resolves (`Ready`/`Eof`/`Interrupted`) or returns a terminal error.
    /// Event-driven — every transition that can flip the probe (segment
    /// write/commit/fail, seek reset, cancel) `signal`s the gate. The
    /// pre-probe [`current`](WaitGate::current) snapshot + park-only-
    /// if-unchanged is a seqlock guard closing the lost-wakeup window even
    /// though the probe predicate and the gate sit under different locks
    /// (mirrors `kithara-storage` `wait_range_inner`). A genuine wedge (no
    /// signal at all) trips the hang watchdog rather than parking forever.
    #[kithara::hang_watchdog(timeout = WAIT_HANG_TIMEOUT)]
    pub(super) fn wait_range_blocking(
        signal: &SizeSignal,
        cancel: &CancelToken,
        mut probe: impl FnMut() -> StreamResult<WaitOutcome>,
    ) -> StreamResult<WaitOutcome> {
        // Cancel is the one transition with no producer-side signal; register a
        // waker that signals the gate so a parked wait observes it. The guard
        // unregisters when this wait returns (mirror storage `wait.rs`).
        let _cancel_wake = {
            let ready = signal.ready_gate();
            cancel.on_cancel(move || ready.signal())
        };
        loop {
            hang_tick!();
            // Snapshot the gate BEFORE the probe: a signal landing between the
            // probe and the park advances the counter, so the park returns at
            // once and we re-probe — no lost wakeup.
            let since = signal.current();
            match probe() {
                Ok(WaitOutcome::Ready) => return Ok(WaitOutcome::Ready),
                Ok(WaitOutcome::Eof) => return Ok(WaitOutcome::Eof),
                Ok(WaitOutcome::Interrupted) => return Ok(WaitOutcome::Interrupted),
                Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => {
                    // Not ready: park on the gate until a signal advances it,
                    // bounded by the re-aim heartbeat.
                }
                Err(e) => return Err(e),
            }
            // Event-driven park: a write/commit/fence/seek/cancel signal wakes
            // us at once to re-probe (the fact of a write, never a timer). If
            // the gate stays quiet for the heartbeat the peer may be mis-aimed
            // after a seek; yield so the off-RT reader re-asserts its prefetch
            // aim and re-enters (mirrors the old per-iteration `notify_peer_wake`
            // without the wall-clock data poll).
            if signal.wait_timeout(since, Self::READER_REAIM_INTERVAL) {
                // Woke from a signal — activity, not a wedge: reset the watchdog.
                hang_reset!();
            } else {
                return Err(StreamError::Source(SourceError::WaitBudgetExceeded));
            }
        }
    }

    delegate! {
        to self.active_session() {
            #[call(variant)]
            pub(crate) fn active(&self) -> Arc<HlsVariant>;
            pub(crate) fn advance(&self, n: u64);
            pub(crate) fn position(&self) -> u64;
            pub(crate) fn seek_time_anchor(
                &self,
                position: Duration,
            ) -> StreamResult<Option<SourceSeekAnchor>>;
            #[call(seek_to_byte)]
            pub(crate) fn set_position(&self, pos: u64);
            /// Variant index of the authoritative active source session.
            pub(crate) fn variant_index(&self) -> usize;
        }
        to self.active() {
            /// Total bytes are >0 - the value used by `Source::len` accessor.
            #[call(stream_len)]
            pub(crate) fn len(&self) -> Option<u64>;
            /// Active variant's media info. `HlsCoord` is constructed
            /// non-empty (asserted in [`Self::new`]) so this always succeeds;
            /// the `Source` trait's `Option<MediaInfo>` shape is restored at
            /// the [`HlsSource`](crate::stream::HlsSource) facade.
            pub(crate) fn media_info(&self) -> MediaInfo;
            /// Collapse the active variant to a fresh single-variant layout on
            /// seek, restoring the natural served range and byte shift.
            #[kithara::probe]
            pub(crate) fn reset_for_seek(&self);
        }
        to self.active().as_ref() {
            pub(crate) fn download_head(&self) -> u32;
            pub(crate) fn format_change_segment_range(&self) -> StreamResult<Range<u64>>;
        }
    }
}

pub(super) fn variant_switch_target_time(
    seek_obs: &dyn SeekObserve,
    playhead_read: &dyn PlayheadRead,
    landing: Option<Duration>,
) -> Duration {
    if seek_obs.is_pending() || seek_obs.is_flushing() {
        return seek_obs
            .target()
            .unwrap_or_else(|| playhead_read.position());
    }
    landing.unwrap_or_else(|| playhead_read.position())
}

/// `VariantControl` exposes the cross-variant fence/format-change surface
/// to the stream layer. The bodies are the coord's existing inherent
/// methods — non-adaptive sources vend `None` instead of implementing
/// these.
impl VariantControl for HlsCoord {
    fn abort_variant(&self, transition: VariantTransition) -> bool {
        Self::abort_variant(self, transition)
    }

    fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        Self::format_change_segment_range(self)
    }

    fn plan_variant_reader(
        &self,
        landing: Option<Duration>,
    ) -> StreamResult<Option<VariantReaderPlan>> {
        Self::plan_variant_reader(self, landing)
    }

    fn prepare_variant_reader(
        &self,
        plan: VariantReaderPlan,
        profile: ReaderProfile,
    ) -> StreamResult<Option<VariantTransition>> {
        Self::prepare_planned_variant_reader(self, plan, profile)
    }

    fn promote_variant(&self, transition: VariantTransition) -> VariantPromotion {
        Self::promote_planned_variant(self, transition)
    }

    fn selected_variant_for_seek(&self) -> usize {
        Self::selected_variant_for_seek(self)
    }

    fn take_prepared_variant_reader(
        &self,
        transition: VariantTransition,
    ) -> StreamResult<VariantReaderTake> {
        Self::take_prepared_variant_reader(self, transition)
    }
}

/// `ByteMap` delegates to the authoritative active session's variant.
///
/// Every method here is a query: [`SeekPrepare::prepare`] rebuilds the layout
/// before the epoch exists, so resolving one on the produce core neither locks
/// nor allocates.
impl ByteMap for HlsCoord {
    delegate! {
        to self {
            #[call(seek_time_anchor)]
            fn anchor_at_time(&self, position: Duration)
            -> StreamResult<Option<SourceSeekAnchor>>;
            #[expr($.init_byte_range())]
            #[call(active)]
            fn init_segment_range(&self) -> Range<u64>;
            #[expr($.stream_len())]
            #[call(active)]
            fn len(&self) -> Option<u64>;
            #[expr($.descriptor_at_byte(byte))]
            #[call(variant_serving)]
            fn segment_at_byte(&self, byte: u64) -> Option<SegmentDescriptor>;
            #[expr(Some($.num_segments()))]
            #[call(active)]
            fn segment_count(&self) -> Option<u32>;
        }
    }

    fn segment_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        self.active().descriptor_after_byte(byte)
    }

    fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
        self.active().descriptor(segment_index as usize)
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.active().descriptor_at_time(t)
    }
}

impl SeekPrepare for HlsCoord {
    delegate! {
        to self {
            #[call(prepare_for_seek)]
            fn prepare(&self);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{ErrorKind, Read},
        num::NonZeroU64,
        sync::OnceLock,
    };

    use kithara_abr::{Abr, AbrController, AbrSettings, AbrState, PendingAbrClaim};
    use kithara_assets::{AssetResource, AssetSource, AssetStore, StorageBackend};
    use kithara_events::{AbrMode, AbrReason, EventBus, RequestPriority, VariantIndex};
    use kithara_platform::{
        sync::{Arc, ThreadGate},
        time::Instant,
    };
    use kithara_stream::{
        AudioCodec, ContainerFormat, PlayheadWrite, ReaderInput, ReaderWarmup, SeekControl,
    };

    use super::*;
    use crate::{
        config::SizeProbeMethod,
        playlist::{PlaylistState, SegmentState, VariantState},
        segment::{MediaSegment, Segment, SegmentContent, SegmentSize, SegmentSlotState},
        variant::{PlanCtx, VariantParts},
    };

    struct TestAbrPeer {
        state: Arc<AbrState>,
        variants: Vec<kithara_events::VariantInfo>,
    }

    impl Abr for TestAbrPeer {
        fn state(&self) -> Option<Arc<AbrState>> {
            Some(Arc::clone(&self.state))
        }

        fn variants(&self) -> Vec<kithara_events::VariantInfo> {
            self.variants.clone()
        }
    }

    fn switch_coord() -> (Arc<HlsCoord>, EventBus, PlanCtx, Arc<AbrState>) {
        let bus = EventBus::new(8);
        let cancel = CancelToken::never();
        let store = Arc::new(
            AssetStore::builder()
                .backend(StorageBackend::Memory)
                .cancel(cancel.clone())
                .build(),
        );
        let signal = SizeSignal::new(Arc::new(ThreadGate::default()), Arc::new(OnceLock::new()));
        let ctx = PlanCtx {
            bus: bus.clone(),
            prefetch_budget: 1,
            scope: store
                .scope::<crate::Hls>(&AssetSource::Remote {
                    url: "https://example.com/master.m3u8"
                        .parse()
                        .expect("master url"),
                    discriminator: Some("coord-test".to_owned()),
                })
                .expect("coord asset scope"),
            seek_epoch: 0,
            look_ahead_bytes: None,
            look_ahead_segments: None,
            headers: None,
            size_probe_method: SizeProbeMethod::Head,
            signal: signal.clone(),
        };
        let playlist = Arc::new(PlaylistState::new(vec![
            VariantState {
                codec: Some(AudioCodec::AacLc),
                container: Some(ContainerFormat::Fmp4),
                init_url: None,
                segments: vec![SegmentState {
                    url: "https://example.com/v0-seg0.m4s".parse().expect("url"),
                    duration: Duration::from_secs(2),
                    byte_range_len: Some(100),
                }],
            },
            VariantState {
                codec: Some(AudioCodec::Mp3),
                container: Some(ContainerFormat::MpegAudio),
                init_url: None,
                segments: vec![SegmentState {
                    url: "https://example.com/v1-seg0.m4s".parse().expect("url"),
                    duration: Duration::from_secs(2),
                    byte_range_len: Some(100),
                }],
            },
        ]));
        let variants: Arc<[Arc<HlsVariant>]> = Arc::from(vec![
            VariantParts {
                init: None,
                segments: vec![Segment::Media(MediaSegment {
                    url: "https://example.com/v0-seg0.m4s".parse().expect("url"),
                    resource_id: ctx
                        .scope
                        .key(&AssetResource::Url(
                            "https://example.com/v0-seg0.m4s".parse().expect("url"),
                        ))
                        .expect("segment key"),
                    state: SegmentSlotState::missing(),
                    size: SegmentSize::seed(100),
                    content: SegmentContent::Plain,
                    decode_time: Duration::ZERO,
                    duration: Duration::from_secs(2),
                })],
                seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
                codec: playlist.variant_codec(0),
                container: playlist.variant_container(0),
            }
            .into_variant(0, &ctx),
            VariantParts {
                init: None,
                segments: vec![Segment::Media(MediaSegment {
                    url: "https://example.com/v1-seg0.m4s".parse().expect("url"),
                    resource_id: ctx
                        .scope
                        .key(&AssetResource::Url(
                            "https://example.com/v1-seg0.m4s".parse().expect("url"),
                        ))
                        .expect("segment key"),
                    state: SegmentSlotState::missing(),
                    size: SegmentSize::seed(100),
                    content: SegmentContent::Plain,
                    decode_time: Duration::ZERO,
                    duration: Duration::from_secs(2),
                })],
                seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
                codec: playlist.variant_codec(1),
                container: playlist.variant_container(1),
            }
            .into_variant(1, &ctx),
        ]);
        let abr_state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
        let abr_publisher = abr_state.publisher();
        let peer: Arc<dyn Abr> = Arc::new(TestAbrPeer {
            state: Arc::clone(&abr_state),
            variants: vec![
                kithara_events::VariantInfo {
                    variant_index: VariantIndex::new(0),
                    bandwidth_bps: Some(128_000),
                    codecs: Some("mp4a.40.2".into()),
                    container: Some("fmp4".into()),
                    duration: kithara_events::VariantDuration::Segmented(vec![
                        Duration::from_secs(2),
                    ]),
                    name: None,
                },
                kithara_events::VariantInfo {
                    variant_index: VariantIndex::new(1),
                    bandwidth_bps: Some(96_000),
                    codecs: Some("mp3".into()),
                    container: Some("mpeg-audio".into()),
                    duration: kithara_events::VariantDuration::Segmented(vec![
                        Duration::from_secs(2),
                    ]),
                    name: None,
                },
            ],
        });
        let controller = Arc::new(AbrController::new(AbrSettings::default()));
        let handle = controller.register(&peer);
        abr_state.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let coord = Arc::new(HlsCoord::new(
            HlsCoordEnv {
                cancel,
                signal,
                scope: ctx.scope.clone(),
                headers: None,
                emit: Arc::new(DeferredBus::new(bus.clone(), 8)),
            },
            Arc::new(PlayheadState::new()),
            Arc::new(SeekState::new()),
            handle,
            abr_publisher,
            variants,
        ));
        (coord, bus, ctx, abr_state)
    }

    fn incremental_profile(read_ahead_bytes: u64) -> ReaderProfile {
        ReaderProfile::new(
            ReaderInput::Incremental,
            ReaderWarmup::None,
            NonZeroU64::new(read_ahead_bytes).expect("non-zero read ahead"),
        )
    }

    /// Mint an intent that supersedes the one already queued for variant 1.
    /// `request_target` keeps its ticket while the target is unchanged — the
    /// ticket names the transition a consumer builds against — so the intent
    /// has to leave and come back for its identity to advance.
    fn renew_switch_intent(abr_state: &AbrState) {
        abr_state.request_target(VariantIndex::new(0), AbrReason::ManualOverride);
        abr_state.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    }

    fn prepare_incoming(
        coord: &HlsCoord,
        profile: ReaderProfile,
    ) -> StreamResult<Option<VariantTransition>> {
        let Some(plan) = VariantControl::plan_variant_reader(coord, None)? else {
            return Ok(None);
        };
        VariantControl::prepare_variant_reader(coord, plan, profile)
    }

    fn take_ready_incremental_reader(
        coord: &HlsCoord,
        ctx: &PlanCtx,
        transition: VariantTransition,
    ) {
        let mut command = coord
            .dispatch_incoming(ctx, 1)
            .pop()
            .expect("incoming fetch");
        command.writer.take().expect("streaming writer")(&[1; 32]).expect("construction bytes");
        assert!(matches!(
            coord
                .take_prepared_variant_reader(transition)
                .expect("take ready reader"),
            VariantReaderTake::Ready(_)
        ));
    }

    #[kithara::test]
    fn variant_switch_target_uses_active_seek_target() {
        let seek = SeekState::new();
        let playhead = PlayheadState::new();
        playhead.set_position(Duration::from_secs(9));

        let _epoch = seek.begin(Duration::from_secs(5));

        assert_eq!(
            variant_switch_target_time(&seek, &playhead, None),
            Duration::from_secs(5)
        );
    }

    #[kithara::test]
    fn variant_switch_target_keeps_flushing_seek_target_after_decoder_applies() {
        let seek = SeekState::new();
        let playhead = PlayheadState::new();
        playhead.set_position(Duration::from_secs(9));

        let epoch = seek.begin(Duration::from_secs(5));
        seek.clear_pending(epoch);

        assert_eq!(
            variant_switch_target_time(&seek, &playhead, None),
            Duration::from_secs(5)
        );
    }

    #[kithara::test]
    fn variant_switch_target_ignores_completed_seek_target() {
        let seek = SeekState::new();
        let playhead = PlayheadState::new();

        let epoch = seek.begin(Duration::from_secs(5));
        seek.clear_pending(epoch);
        seek.complete(epoch);
        playhead.set_position(Duration::from_secs(9));

        assert_eq!(
            variant_switch_target_time(&seek, &playhead, None),
            Duration::from_secs(9)
        );
    }

    #[kithara::test]
    fn variant_switch_target_prefers_the_named_landing_over_the_playhead() {
        let seek = SeekState::new();
        let playhead = PlayheadState::new();
        playhead.set_position(Duration::from_secs(9));

        assert_eq!(
            variant_switch_target_time(&seek, &playhead, Some(Duration::from_secs(12))),
            Duration::from_secs(12)
        );
    }

    #[kithara::test]
    fn variant_switch_target_keeps_the_seek_target_over_a_named_landing() {
        let seek = SeekState::new();
        let playhead = PlayheadState::new();
        playhead.set_position(Duration::from_secs(9));

        let _epoch = seek.begin(Duration::from_secs(5));

        assert_eq!(
            variant_switch_target_time(&seek, &playhead, Some(Duration::from_secs(12))),
            Duration::from_secs(5)
        );
    }

    #[kithara::test]
    fn exact_session_starts_with_one_resident() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();

        assert_eq!(coord.sessions.resident_count(), 1);
        assert_eq!(coord.variant_index(), 0);
    }

    #[kithara::test]
    fn variant_reader_plan_exposes_target_facts_before_session_creation() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();

        let plan = coord
            .plan_variant_reader(None)
            .expect("plan incoming")
            .expect("pending switch");

        assert_eq!(plan.transition().active_variant(), VariantIndex::new(0));
        assert_eq!(plan.transition().incoming_variant(), VariantIndex::new(1));
        assert_eq!(plan.media_info().codec, Some(AudioCodec::Mp3));
        assert_eq!(
            plan.media_info().container,
            Some(ContainerFormat::MpegAudio)
        );
        assert_eq!(plan.media_info().variant_index, Some(1));
        assert_eq!(plan.landing_time(), Duration::ZERO);
        assert_eq!(
            coord.sessions.resident_count(),
            1,
            "planning must not open the incoming session"
        );
    }

    #[kithara::test]
    fn stale_variant_reader_plan_cannot_open_a_newer_intent() {
        let (coord, _bus, _ctx, abr_state) = switch_coord();
        let stale = coord
            .plan_variant_reader(None)
            .expect("plan first incoming")
            .expect("first pending switch");

        renew_switch_intent(&abr_state);
        let current = coord
            .plan_variant_reader(None)
            .expect("plan replacement incoming")
            .expect("replacement pending switch");

        assert_ne!(stale.transition().id(), current.transition().id());
        assert_eq!(
            coord
                .prepare_planned_variant_reader(stale, incremental_profile(32))
                .expect("reject stale plan"),
            None
        );
        assert_eq!(coord.sessions.resident_count(), 1);
        assert_eq!(
            coord
                .prepare_planned_variant_reader(current.clone(), incremental_profile(32))
                .expect("prepare current plan"),
            Some(current.transition())
        );
        assert_eq!(coord.sessions.resident_count(), 2);
        assert!(coord.abort_variant(current.transition()));
    }

    #[kithara::test]
    fn promotion_distinguishes_deferred_from_stale_transition() {
        let (coord, _bus, _ctx, abr_state) = switch_coord();
        let plan = coord
            .plan_variant_reader(None)
            .expect("plan incoming")
            .expect("pending switch");
        let transition = plan.transition();
        coord
            .prepare_planned_variant_reader(plan, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");

        assert_eq!(
            coord.promote_planned_variant(transition),
            VariantPromotion::Deferred
        );

        renew_switch_intent(&abr_state);
        assert_eq!(
            coord.promote_planned_variant(transition),
            VariantPromotion::Stale
        );
        assert_eq!(coord.sessions.resident_count(), 1);
        assert_eq!(coord.variant_index(), 0);
    }

    #[kithara::test]
    fn incoming_session_does_not_publish_variant_or_cursor_before_promotion() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        coord.set_position(17);

        let transition = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        assert_eq!(coord.sessions.resident_count(), 2);

        assert_eq!(coord.variant_index(), 0);
        assert_eq!(coord.position(), 17);
        assert!(matches!(
            coord
                .take_prepared_variant_reader(transition)
                .expect("probe incoming"),
            VariantReaderTake::Preparing
        ));
        assert!(coord.abort_variant(transition));
        assert_eq!(coord.sessions.resident_count(), 1);
        assert_eq!(coord.variant_index(), 0);
        assert_eq!(coord.position(), 17);
    }

    #[kithara::test]
    fn exact_session_resolution_follows_the_committed_abr_selector_before_collapse() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        let claim = coord
            .abr
            .claim_pending_decision()
            .expect("prepared transition keeps its exact claim");

        assert!(coord.abr_publisher.commit_pending(claim, Instant::now()));

        assert_eq!(
            coord.active_session().variant_index(),
            1,
            "the committed ABR selector must resolve through the resident pair before collapse"
        );
    }

    #[kithara::test]
    fn exact_session_resolution_retries_a_stale_selector_snapshot() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        let mut selector_observations = [0, 1, 1, 1].into_iter();

        let resolved = coord.sessions.active(|| {
            selector_observations
                .next()
                .expect("resolver performs two selector reads per attempt")
        });

        assert_eq!(resolved.variant_index(), 1);
        assert_eq!(selector_observations.next(), None);
    }

    #[kithara::test(tokio)]
    async fn incoming_readiness_tracks_required_bytes_not_delivery_chunks() {
        let (coord, _bus, ctx, _abr_state) = switch_coord();
        coord.set_position(17);
        let transition = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        let mut commands = coord.dispatch_incoming(&ctx, 1);
        assert_eq!(commands.len(), 1);
        let mut command = commands.pop().expect("target fetch");
        // Construction fetches outrank speculation. The peer's budget decides
        // how many a poll may emit; it cannot decide when the downloader runs
        // them, and that queue is first-in-first-out within a priority slot. The
        // audible variant appends to it on every poll, so an untagged
        // construction pack is never reached: measured on a cross-codec switch,
        // the incoming variant started exactly two fetches all test — its
        // playlist and its init — while both of its media segments sat queued
        // until teardown and readiness stayed false on all 216 polls. Tagged,
        // both segments start and the reader becomes readable.
        assert_eq!(command.priority, Some(RequestPriority::High));
        assert_eq!(command.url.as_str(), "https://example.com/v1-seg0.m4s");
        let mut writer = command.writer.take().expect("streaming writer");

        writer(&[1; 7]).expect("first delivery chunk");
        assert!(matches!(
            coord
                .take_prepared_variant_reader(transition)
                .expect("probe partial incoming"),
            VariantReaderTake::Preparing
        ));

        writer(&[2; 25]).expect("second delivery chunk");
        let VariantReaderTake::Ready(reader) = coord
            .take_prepared_variant_reader(transition)
            .expect("take ready incoming")
        else {
            panic!("reader must become ready at the declared byte requirement");
        };
        assert_eq!(reader.transition(), transition);
        assert_eq!(reader.media_info().variant_index, Some(1));
        assert_eq!(reader.landing_time(), Duration::ZERO);
        let (plan, reader) = reader.split();
        assert_eq!(plan.transition(), transition);
        assert_eq!(plan.media_info().variant_index, Some(1));
        assert_eq!(plan.landing_time(), Duration::ZERO);
        let mut input = reader.into_inner();
        assert!(matches!(
            coord
                .take_prepared_variant_reader(transition)
                .expect("take reader twice"),
            VariantReaderTake::Taken
        ));

        writer(&[3; 68]).expect("remaining delivery body");
        command.on_complete.take().expect("completion handler")(100, None, None);
        let mut first = [0_u8; 8];
        assert_eq!(input.read(&mut first).expect("read incoming"), first.len());
        assert_eq!(coord.position(), 17);
        assert_eq!(
            coord.promote_planned_variant(transition),
            VariantPromotion::Promoted
        );
        assert_eq!(coord.sessions.resident_count(), 1);
        assert_eq!(coord.variant_index(), 1);
        assert_eq!(coord.position(), first.len() as u64);

        let mut second = [0_u8; 8];
        assert_eq!(
            input.read(&mut second).expect("read promoted session"),
            second.len()
        );
        assert_eq!(coord.position(), (first.len() + second.len()) as u64);
    }

    #[kithara::test]
    fn promotion_requires_the_exact_taken_reader() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let transition = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");

        assert_eq!(
            coord.promote_planned_variant(transition),
            VariantPromotion::Deferred
        );
        assert_eq!(coord.variant_index(), 0);
        assert!(coord.abort_variant(transition));
    }

    #[kithara::test]
    fn one_transition_rejects_a_different_reader_profile() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let transition = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");

        let error = prepare_incoming(&coord, incremental_profile(64))
            .expect_err("profile mismatch must fail");
        assert!(matches!(
            error,
            StreamError::Source(SourceError::Io(ref error))
                if error.kind() == ErrorKind::InvalidInput
        ));
        assert!(matches!(
            coord
                .take_prepared_variant_reader(transition)
                .expect("original transition remains live"),
            VariantReaderTake::Preparing
        ));
        assert!(coord.abort_variant(transition));
    }

    #[kithara::test]
    fn stale_same_target_identity_cannot_disturb_newer_incoming() {
        let (coord, _bus, ctx, abr_state) = switch_coord();
        let stale = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare first incoming")
            .expect("first pending switch");
        let mut stale_command = coord
            .dispatch_incoming(&ctx, 1)
            .pop()
            .expect("first session fetch");
        let stale_cancel = stale_command
            .cancel
            .as_ref()
            .expect("session fetch cancel")
            .clone();
        assert!(!stale_cancel.is_cancelled());

        renew_switch_intent(&abr_state);
        let current = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare replacement incoming")
            .expect("replacement pending switch");

        assert_ne!(stale.id(), current.id());
        assert!(stale_cancel.is_cancelled());
        stale_command.on_complete.take().expect("stale completion")(0, None, None);
        let current_command = coord
            .dispatch_incoming(&ctx, 1)
            .pop()
            .expect("replacement session fetch");
        let current_cancel = current_command
            .cancel
            .as_ref()
            .expect("replacement fetch cancel")
            .clone();
        assert!(!current_cancel.is_cancelled());
        assert_eq!(
            coord.promote_planned_variant(stale),
            VariantPromotion::Stale
        );
        assert!(!coord.abort_variant(stale));
        assert!(!current_cancel.is_cancelled());
        assert!(matches!(
            coord
                .take_prepared_variant_reader(current)
                .expect("replacement remains live"),
            VariantReaderTake::Preparing
        ));
        assert!(coord.abort_variant(current));
        assert!(current_cancel.is_cancelled());
    }

    #[kithara::test]
    fn aborted_exact_intent_keeps_its_claim_without_switching() {
        let (coord, _bus, _ctx, abr_state) = switch_coord();
        let transition = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        assert!(coord.abort_variant(transition));

        abr_state.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        assert!(!coord.has_incoming());
        assert_eq!(coord.variant_index(), 0);
        assert!(coord.abr.claim_pending_decision().is_some());
    }

    #[kithara::test]
    fn locked_exact_intent_keeps_its_incoming_session() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let plan = coord
            .plan_variant_reader(None)
            .expect("plan incoming")
            .expect("pending switch");
        let transition = plan.transition();
        let landing_time = plan.landing_time();
        coord
            .prepare_planned_variant_reader(plan, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        coord.playhead_write().set_position(Duration::from_secs(9));

        coord.abr.lock();
        let locked = coord
            .plan_variant_reader(None)
            .expect("locked plan")
            .expect("locked incoming");
        assert_eq!(locked.landing_time(), landing_time);
        assert_eq!(
            coord
                .prepare_planned_variant_reader(locked, incremental_profile(32))
                .expect("locked preparation probe"),
            Some(transition)
        );
        assert!(coord.has_incoming());

        coord.abr.unlock();
        let ready = coord
            .plan_variant_reader(None)
            .expect("ready plan")
            .expect("ready incoming");
        assert_eq!(ready.landing_time(), landing_time);
        assert_eq!(
            coord
                .prepare_planned_variant_reader(ready, incremental_profile(32))
                .expect("unlocked preparation probe"),
            Some(transition)
        );
    }

    #[kithara::test]
    fn locked_exact_intent_rejects_a_reader_profile_change() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        coord.abr.lock();

        let error = prepare_incoming(&coord, incremental_profile(64))
            .expect_err("one transition cannot change reader profile while locked");

        assert!(matches!(
            error,
            StreamError::Source(SourceError::Io(ref error))
                if error.kind() == ErrorKind::InvalidInput
        ));
        assert!(coord.has_incoming());
    }

    #[kithara::test]
    fn locked_exact_intent_rejects_a_different_reader_plan() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let plan = coord
            .plan_variant_reader(None)
            .expect("plan incoming")
            .expect("pending switch");
        let transition = plan.transition();
        let media_info = plan.media_info().clone();
        coord
            .prepare_planned_variant_reader(plan, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        coord.abr.lock();

        let mismatched = VariantReaderPlan::new(transition, media_info, Duration::from_secs(1));
        let error = coord
            .prepare_planned_variant_reader(mismatched, incremental_profile(32))
            .expect_err("one transition cannot change reader plan while locked");

        assert!(matches!(
            error,
            StreamError::Source(SourceError::Io(ref error))
                if error.kind() == ErrorKind::InvalidInput
        ));
        assert!(coord.has_incoming());
    }

    #[kithara::test]
    fn locked_plan_drops_an_incoming_from_an_old_seek_epoch() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let plan = coord
            .plan_variant_reader(None)
            .expect("plan incoming")
            .expect("pending switch");
        coord
            .prepare_planned_variant_reader(plan, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        coord.abr.lock();

        let _epoch = coord.seek_control().begin(Duration::from_secs(1));

        assert_eq!(
            coord.plan_variant_reader(None).expect("reject old epoch"),
            None
        );
        assert_eq!(coord.sessions.resident_count(), 1);
        assert_eq!(coord.selected_variant_for_seek(), 1);
    }

    #[kithara::test]
    fn ready_stale_plan_drops_old_epoch_incoming_without_deleting_selection() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let plan = coord
            .plan_variant_reader(None)
            .expect("plan incoming")
            .expect("pending switch");
        let stale = plan.clone();
        coord
            .prepare_planned_variant_reader(plan, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        let claim = coord
            .abr
            .claim_pending_decision()
            .expect("selection remains pending until promotion");

        let _epoch = coord.seek_control().begin(Duration::from_secs(1));

        assert_eq!(
            coord
                .prepare_planned_variant_reader(stale, incremental_profile(32))
                .expect("reject old epoch"),
            None
        );
        assert_eq!(coord.sessions.resident_count(), 1);
        assert_eq!(coord.abr.claim_pending_decision(), Some(claim));
        assert_eq!(coord.selected_variant_for_seek(), 1);
    }

    #[kithara::test]
    fn exact_session_mode_accepts_intent_created_before_coord() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let plan = coord
            .plan_variant_reader(None)
            .expect("plan pre-existing exact intent")
            .expect("fixture requested target before coord construction");

        assert_eq!(plan.transition().active_variant(), VariantIndex::new(0));
        assert_eq!(plan.transition().incoming_variant(), VariantIndex::new(1));
        assert_eq!(coord.variant_index(), 0);
    }

    #[kithara::test]
    fn terminal_exact_preparation_error_aborts_its_claim() {
        let (coord, _bus, _ctx, abr_state) = switch_coord();
        abr_state.set_mode(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr_state.request_target(VariantIndex::new(9), AbrReason::UpSwitch);

        let error = prepare_incoming(&coord, incremental_profile(32))
            .expect_err("unknown target must fail");
        assert!(matches!(
            error,
            StreamError::Source(SourceError::VariantNotFound(_))
        ));
        assert!(coord.abr.claim_pending_decision().is_none());
        assert_eq!(coord.variant_index(), 0);
    }

    #[kithara::test]
    fn active_seek_anchor_updates_the_session_cursor() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        coord.set_position(17);

        let anchor = ByteMap::anchor_at_time(coord.as_ref(), Duration::from_secs(1))
            .expect("resolve seek anchor")
            .expect("segmented source anchor");

        assert_eq!(anchor.byte_offset, 0);
        assert_eq!(coord.position(), anchor.byte_offset);
    }

    #[kithara::test]
    fn seek_aborts_the_exact_incoming_session() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let transition = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");

        let _epoch = coord.seek_control().begin(Duration::from_secs(1));
        coord.prepare_for_seek();

        assert!(matches!(
            coord
                .take_prepared_variant_reader(transition)
                .expect("probe stale incoming"),
            VariantReaderTake::Stale
        ));
        assert_eq!(coord.variant_index(), 0);
    }

    #[kithara::test]
    fn seek_replaces_the_session_without_deleting_manual_selection() {
        let (coord, _bus, _ctx, _abr_state) = switch_coord();
        let claim = coord
            .abr
            .claim_pending_decision()
            .expect("manual selection claim");
        let stale = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");

        let next_epoch = coord.seek_control().begin(Duration::from_secs(1));
        coord.prepare_for_seek();

        assert_eq!(coord.abr.claim_pending_decision(), Some(claim));
        let replacement = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare replacement")
            .expect("manual selection survives seek");
        assert_eq!(replacement.id().abr_ticket(), stale.id().abr_ticket());
        assert_eq!(replacement.id().seek_epoch(), next_epoch);
        assert_ne!(replacement, stale);
    }

    #[kithara::test]
    fn seek_replaces_the_session_without_deleting_automatic_selection() {
        let (coord, _bus, _ctx, abr_state) = switch_coord();
        abr_state.set_mode(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr_state.request_target(VariantIndex::new(1), AbrReason::UpSwitch);
        let claim = coord
            .abr
            .claim_pending_decision()
            .expect("automatic selection claim");
        let stale = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");

        let next_epoch = coord.seek_control().begin(Duration::from_secs(1));
        coord.prepare_for_seek();

        assert_eq!(coord.abr.claim_pending_decision(), Some(claim));
        let replacement = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare replacement")
            .expect("automatic selection survives seek");
        assert_eq!(replacement.id().abr_ticket(), stale.id().abr_ticket());
        assert_eq!(replacement.id().seek_epoch(), next_epoch);
        assert_ne!(replacement, stale);
    }

    #[kithara::test]
    fn seek_selection_reads_the_pending_target_while_abr_is_locked() {
        let (coord, _bus, _ctx, abr_state) = switch_coord();
        abr_state.set_mode(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr_state.request_target(VariantIndex::new(1), AbrReason::UpSwitch);
        coord.abr.lock();

        assert_eq!(coord.selected_variant_for_seek(), 1);
        assert!(matches!(
            coord.abr.pending_claim(),
            PendingAbrClaim::Locked(_)
        ));
    }

    #[kithara::test(tokio)]
    async fn seek_epoch_rejects_taken_generation_and_preserves_its_selection() {
        let (coord, _bus, ctx, _abr_state) = switch_coord();
        let claim = coord
            .abr
            .claim_pending_decision()
            .expect("manual selection claim");
        let stale = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare incoming")
            .expect("pending switch");
        take_ready_incremental_reader(&coord, &ctx, stale);

        let next_epoch = coord.seek_control().begin(Duration::from_secs(1));

        assert_eq!(
            coord.promote_planned_variant(stale),
            VariantPromotion::Stale
        );
        assert_eq!(coord.variant_index(), 0);
        assert_eq!(coord.abr.claim_pending_decision(), Some(claim));
        let replacement = prepare_incoming(&coord, incremental_profile(32))
            .expect("prepare replacement")
            .expect("manual selection survives epoch change");
        assert_eq!(replacement.id().abr_ticket(), stale.id().abr_ticket());
        assert_eq!(replacement.id().seek_epoch(), next_epoch);
    }
}
