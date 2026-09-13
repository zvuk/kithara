use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use bitflags::bitflags;
use kithara_platform::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use num_traits::ToPrimitive;

use super::{
    decision::AbrDecision,
    pending::{AbrTicket, PendingAbrClaim, PendingAbrDecision},
    publisher::AbrPublisher,
    view::AbrView,
};
use crate::{AbrMode, AbrReason, VariantIndex};

bitflags! {
    /// Composable boolean control-state for [`AbrState`], orthogonal to the
    /// `mode` (Auto/Manual, carries an index) and the reentrant `lock` count.
    /// Lives in its own [`AtomicU8`] — room to grow as more escape-class
    /// states are added.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct AbrFlags: u8 {
        /// The active variant cannot deliver the segment the reader is blocked
        /// on (set by the HLS stall detector). While set, `evaluate()` excludes
        /// the current variant from candidacy and skips the buffer-too-low
        /// up-switch gate — staying cannot grow the buffer.
        const ESCAPE = 1 << 0;
    }
}

/// Per-peer ABR state owned by a peer and shared with the controller.
///
/// Variants live in the peer (single source of truth) and reach the
/// state's `decide()` via [`AbrView::variants`]. The state itself only
/// tracks runtime control: current index, mode, switch timing, locks,
/// pending boundary commit.
pub struct AbrState {
    current_variant: Arc<AtomicUsize>,
    last_switch_at_nanos: AtomicU64,
    max_bandwidth_bps: AtomicU64,
    flags: AtomicU8,
    lock_count: AtomicUsize,
    mode: AtomicUsize,
    reference_instant: Instant,
    /// Phase 2 boundary-commit slot. `Some` means a switch has been
    /// requested via [`request_target`](AbrState::request_target) and
    /// the scheduler has not yet observed it on a segment boundary.
    /// Replace-pending semantics: a fresh `request_target` overwrites
    /// any prior unobserved entry (latest-wins, matching the
    /// "switch-only-on-boundaries" contract from the two-cursor plan).
    pending: Mutex<PendingState>,
}

#[derive(Debug)]
struct PendingState {
    pending: Option<PendingApply>,
    next_ticket: u64,
}

impl Default for PendingState {
    fn default() -> Self {
        Self {
            next_ticket: 1,
            pending: None,
        }
    }
}

impl PendingState {
    /// Hand out the identity of one transition attempt. Two intents that
    /// share a ticket are indistinguishable to every holder of it, so a
    /// slot written for a new reason takes a new ticket.
    ///
    /// # Panics
    ///
    /// Panics after exhausting the monotonic ABR ticket space.
    fn mint_ticket(&mut self) -> AbrTicket {
        assert!(self.next_ticket < u64::MAX, "ABR ticket space exhausted");
        let ticket = AbrTicket::new(self.next_ticket);
        self.next_ticket += 1;
        ticket
    }
}

/// Captured intent of a pending switch: the target variant index plus
/// the reason the requestor (controller, manual UI, scheduler) wants
/// recorded once the boundary commit lands.
#[derive(Clone, Copy, Debug)]
struct PendingApply {
    reason: AbrReason,
    ticket: AbrTicket,
    target: VariantIndex,
}

/// `true` for `AbrReason` variants that originate from a throughput
/// estimate — the ones that go stale across a position jump and must
/// not survive a seek into the post-unlock boundary commit. Manual or
/// first-pick reasons stay valid through a seek and are preserved.
const fn is_throughput_driven(reason: AbrReason) -> bool {
    matches!(
        reason,
        AbrReason::UpSwitch
            | AbrReason::DownSwitch
            | AbrReason::UrgentDownSwitch
            | AbrReason::EscapeStalled
    )
}

/// A switch the active variant's failure forced, rather than one taken to
/// improve quality. Its premise is that the variant stopped delivering, and
/// no amount of throughput evidence speaks to that: an estimate recovers as
/// soon as some *other* variant's segment lands, while the stalled one is
/// still stalled.
pub(super) const fn is_rescue(reason: AbrReason) -> bool {
    matches!(
        reason,
        AbrReason::EscapeStalled | AbrReason::UrgentDownSwitch
    )
}

/// Rebuild the typed [`AbrDecision`] a pending boundary-commit publishes,
/// using the live `from` and the reason captured at `request_target`. The
/// only reasons that reach a pending slot are switch-class (a `Stay` never
/// requests a target), so a non-down, non-manual reason classifies as an
/// up-switch.
const fn pending_decision(from: VariantIndex, to: VariantIndex, reason: AbrReason) -> AbrDecision {
    match reason {
        AbrReason::ManualOverride => AbrDecision::Manual { from, to },
        AbrReason::DownSwitch | AbrReason::UrgentDownSwitch => {
            AbrDecision::DownSwitch { from, to, reason }
        }
        _ => AbrDecision::UpSwitch { from, to, reason },
    }
}

impl AbrState {
    const NO_BANDWIDTH_CAP: u64 = 0;
    const NO_SWITCH: u64 = 0;

    /// Build an `AbrState` with the initial variant set from `mode`.
    #[must_use]
    pub fn new(mode: AbrMode) -> Self {
        Self::new_at(mode, Instant::now())
    }

    /// Drop the pending request only when `ticket` still identifies it.
    ///
    /// Returns `true` when the matching request was removed. A stale ticket
    /// leaves a newer request untouched.
    #[must_use]
    pub fn abort_pending(&self, ticket: AbrTicket) -> bool {
        let mut state = self.pending.lock();
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.ticket != ticket)
        {
            return false;
        }
        state.pending = None;
        true
    }

    /// Publish the switch: a [`AbrDecision::Stay`] is a no-op; otherwise
    /// atomically clear the pending slot **iff** it still references the
    /// same target as `decision`, then store `current_variant :=
    /// decision.target()` and record the switch timestamp.
    ///
    /// The "iff" rule preserves the replace-pending semantic: if an
    /// external `request_target` overwrote the slot with a different
    /// target between [`peek_pending_decision`](Self::peek_pending_decision)
    /// and this call, the new pending stays untouched and the next
    /// boundary commits it. The captured `decision` still publishes —
    /// the caller has already prepared `v_new` for that target.
    pub fn apply_decision(&self, decision: &AbrDecision, now: Instant) {
        if !decision.changed() {
            return;
        }
        let target = decision.target();
        let mut state = self.pending.lock();
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.target == target)
        {
            state.pending = None;
        }
        self.clear_escape();
        self.current_variant.store(target.get(), Ordering::Release);
        self.record_switch(now);
        drop(state);
    }

    /// Claim the exact pending request without consuming it.
    ///
    /// Returns `None` while locked, when no request exists, or when the
    /// pending target already equals `current`.
    #[must_use]
    pub fn claim_pending_decision(&self, current: VariantIndex) -> Option<PendingAbrDecision> {
        match self.pending_claim(current) {
            PendingAbrClaim::Ready(claim) => Some(claim),
            PendingAbrClaim::Absent | PendingAbrClaim::Locked(_) => None,
        }
    }

    /// Clear the escape condition — the active variant is delivering again, or
    /// a switch off it has been published. Idempotent.
    pub fn clear_escape(&self) {
        self.flags
            .fetch_and(!AbrFlags::ESCAPE.bits(), Ordering::AcqRel);
    }

    /// Publish `claim` only when it still identifies the pending request.
    ///
    /// Returns `true` after publishing and consuming the matching request. A
    /// stale claim leaves both the audible variant and any newer intent
    /// untouched.
    #[must_use]
    pub fn commit_pending(&self, claim: PendingAbrDecision, now: Instant) -> bool {
        let mut state = self.pending.lock();
        if self.is_locked() {
            return false;
        }
        let Some(pending) = state.pending else {
            return false;
        };
        let current = self.current_variant_index();
        let decision = pending_decision(current, pending.target, pending.reason);
        if pending.ticket != claim.ticket() || decision != claim.decision() {
            return false;
        }

        state.pending = None;
        let target = decision.target();
        self.clear_escape();
        self.current_variant.store(target.get(), Ordering::Release);
        self.record_switch(now);
        drop(state);
        true
    }

    #[must_use]
    pub fn current_variant_index(&self) -> VariantIndex {
        VariantIndex::new(self.current_variant.load(Ordering::Acquire))
    }

    /// Produce a decision without mutating state.
    #[must_use]
    pub fn decide(&self, view: &AbrView<'_>, now: Instant) -> AbrDecision {
        super::decision::evaluate(self, view, now)
    }

    fn instant_to_nanos(&self, instant: Instant) -> u64 {
        let nanos = instant
            .saturating_duration_since(self.reference_instant)
            .as_nanos()
            .to_u64()
            .unwrap_or(u64::MAX);
        nanos.max(1)
    }

    /// Drop a throughput-driven pending boundary-commit. Called when a
    /// new seek epoch arrives (`HlsPeer::apply_seek_change`): a pending
    /// up-switch chosen against pre-seek throughput becomes stale once
    /// the reader jumps, and would otherwise commit on the first
    /// boundary cross after the seek lands — forcing a decoder recreate
    /// while the new-variant cache is still empty (the prod `app.log`
    /// `HangDetector` signature). After the seek, `decide()`
    /// re-evaluates against post-seek throughput before any new auto
    /// switch is requested.
    ///
    /// `AbrReason::ManualOverride` and `AbrReason::Initial` are
    /// preserved — they encode user-driven or first-pick intent that
    /// is not invalidated by a position jump.
    ///
    /// Distinct from [`Self::lock`]: locking gates *publish* but
    /// preserves the pending intent so it can resume on unlock (the
    /// blender-fence contract codified in
    /// `peek_pending_decision_returns_none_during_seek`). Invalidating
    /// is destructive and happens only at semantic seek boundaries.
    pub fn invalidate_pending(&self) {
        let mut state = self.pending.lock();
        if state
            .pending
            .as_ref()
            .is_some_and(|p| is_throughput_driven(p.reason))
        {
            state.pending = None;
        }
    }

    /// `true` while the active variant is flagged non-delivering. Read by
    /// `evaluate()` to exclude the variant and skip the buffer up-switch gate.
    #[must_use]
    pub fn is_escaping(&self) -> bool {
        AbrFlags::from_bits_truncate(self.flags.load(Ordering::Acquire)).contains(AbrFlags::ESCAPE)
    }

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.lock_count.load(Ordering::Acquire) > 0
    }

    /// Gate publication of pending decisions: while `lock_count > 0`,
    /// [`peek_pending_decision`](Self::peek_pending_decision) returns
    /// `None` so the boundary commit defers until unlock. The pending
    /// intent itself is preserved across lock/unlock — destructive
    /// invalidation of stale pre-seek intents is the job of
    /// [`invalidate_pending`](Self::invalidate_pending), called from
    /// `coord::reset_for_seek` on a semantic seek boundary.
    pub fn lock(&self) {
        let state = self.pending.lock();
        self.lock_count.fetch_add(1, Ordering::AcqRel);
        drop(state);
    }

    #[must_use]
    pub fn lock_count(&self) -> usize {
        self.lock_count.load(Ordering::Acquire)
    }

    /// Flag the active variant as non-delivering — set by the HLS stall
    /// detector when the reader is parked at a clean boundary on a segment
    /// whose in-flight fetch has crossed the downloader's `soft_timeout`.
    /// Idempotent. The caller triggers an out-of-band re-tick (see
    /// [`AbrHandle::reevaluate`](crate::AbrHandle::reevaluate)) so `evaluate()`
    /// observes it.
    pub fn mark_escape(&self) {
        self.flags
            .fetch_or(AbrFlags::ESCAPE.bits(), Ordering::AcqRel);
    }

    #[must_use]
    pub fn max_bandwidth_bps(&self) -> Option<u64> {
        let v = self.max_bandwidth_bps.load(Ordering::Acquire);
        if v == Self::NO_BANDWIDTH_CAP {
            None
        } else {
            Some(v)
        }
    }

    #[must_use]
    pub fn mode(&self) -> AbrMode {
        AbrMode::from(self.mode.load(Ordering::Acquire))
    }

    /// Build an `AbrState` whose session starts at `reference`.
    ///
    /// The anti-oscillation interval before the first switch is measured from
    /// the session's start, so a test that states what that interval holds has
    /// to be able to say when the session started - otherwise the assertion is
    /// a race against its own setup.
    #[must_use]
    pub(crate) fn new_at(mode: AbrMode, reference: Instant) -> Self {
        let initial_variant = match mode {
            AbrMode::Auto(Some(idx)) | AbrMode::Manual(idx) => idx.get(),
            AbrMode::Auto(None) => 0,
        };
        Self {
            current_variant: Arc::new(AtomicUsize::new(initial_variant)),
            last_switch_at_nanos: AtomicU64::new(Self::NO_SWITCH),
            max_bandwidth_bps: AtomicU64::new(Self::NO_BANDWIDTH_CAP),
            lock_count: AtomicUsize::new(0),
            mode: AtomicUsize::new(mode.into()),
            flags: AtomicU8::new(AbrFlags::empty().bits()),
            reference_instant: reference,
            pending: Mutex::default(),
        }
    }

    /// Observe whether an exact pending intent is absent, temporarily locked,
    /// or ready to claim.
    #[must_use]
    pub fn pending_claim(&self, current: VariantIndex) -> PendingAbrClaim {
        let state = self.pending.lock();
        let Some(pending) = state.pending else {
            return PendingAbrClaim::Absent;
        };
        if pending.target == current {
            return PendingAbrClaim::Absent;
        }
        let claim = PendingAbrDecision::new(
            pending.ticket,
            pending_decision(current, pending.target, pending.reason),
        );
        let locked = self.is_locked();
        drop(state);
        if locked {
            return PendingAbrClaim::Locked(claim);
        }
        PendingAbrClaim::Ready(claim)
    }

    /// Phase 2 read-only view of the unobserved pending switch (if any).
    /// Used by the Phase 3 scheduler boundary check and by tests.
    #[must_use]
    pub fn pending_target(&self) -> Option<VariantIndex> {
        self.pending
            .lock()
            .pending
            .as_ref()
            .map(|pending| pending.target)
    }

    /// Mint the publication capability retained by this state's source owner.
    #[must_use]
    pub fn publisher(self: &Arc<Self>) -> AbrPublisher {
        AbrPublisher::new(Arc::clone(self))
    }

    fn record_switch(&self, now: Instant) {
        self.last_switch_at_nanos
            .store(self.instant_to_nanos(now), Ordering::Release);
    }

    /// Replaces the pending boundary switch unless the same target is already queued.
    /// Manual mode accepts only its pinned target.
    ///
    /// # Panics
    ///
    /// Panics after exhausting the monotonic ABR ticket space.
    pub fn request_target(&self, target: VariantIndex, reason: AbrReason) {
        let mut state = self.pending.lock();
        if let AbrMode::Manual(idx) = self.mode()
            && idx != target
        {
            return;
        }
        if state
            .pending
            .is_some_and(|pending| pending.target == target)
        {
            return;
        }
        let ticket = state.mint_ticket();
        state.pending = Some(PendingApply {
            reason,
            ticket,
            target,
        });
    }

    /// Drop a throughput-driven pending whose target the live decision no
    /// longer wants. Called from the controller tick's
    /// `Stay { AlreadyOptimal }` arm — the one verdict that re-affirms
    /// `current` against fresh evidence. Without it, an urgent down-switch
    /// latched on the initial throughput seed outlives the estimate that
    /// justified it and commits a quality drop at the next boundary.
    ///
    /// Narrower than [`invalidate_pending`](Self::invalidate_pending), which
    /// drops every throughput-driven pending across a position jump: manual and
    /// first-pick intents are not throughput claims, and a rescue is not one
    /// either — see [`is_rescue`]. A pending that already targets `current`
    /// describes no divergence and is left untouched.
    pub(crate) fn retract_throughput_pending(&self, current: VariantIndex) {
        let mut state = self.pending.lock();
        if state.pending.as_ref().is_some_and(|p| {
            is_throughput_driven(p.reason) && !is_rescue(p.reason) && p.target != current
        }) {
            state.pending = None;
        }
    }

    /// Variant a seek replacement must open. A pending selection intent wins
    /// even while ABR publication is locked.
    #[must_use]
    pub fn selected_variant_for_seek(&self) -> VariantIndex {
        self.pending
            .lock()
            .pending
            .as_ref()
            .map_or_else(|| self.current_variant_index(), |pending| pending.target)
    }

    pub fn set_max_bandwidth_bps(&self, cap: Option<u64>) {
        self.max_bandwidth_bps
            .store(cap.unwrap_or(Self::NO_BANDWIDTH_CAP), Ordering::Release);
    }

    /// Applies a validated mode and clears any superseded pending switch.
    ///
    /// A manual pin on the target the slot already holds keeps that intent
    /// alive, but under a fresh ticket. The ticket is the identity of one
    /// transition attempt, and the slot it finds may already be claimed:
    /// reusing the ticket would let that attempt's abort cancel the command
    /// the listener has just given. Re-pinning what is already a manual
    /// override on that target restates nothing and keeps its ticket.
    ///
    /// # Panics
    ///
    /// Panics after exhausting the monotonic ABR ticket space.
    pub fn set_mode(&self, mode: AbrMode) {
        let mut state = self.pending.lock();
        self.mode.store(mode.into(), Ordering::Release);
        let restated = match mode {
            AbrMode::Manual(target) => state.pending.filter(|pending| pending.target == target),
            AbrMode::Auto(_) => None,
        };
        match restated {
            Some(pending) if matches!(pending.reason, AbrReason::ManualOverride) => {}
            Some(pending) => {
                let ticket = state.mint_ticket();
                state.pending = Some(PendingApply {
                    reason: AbrReason::ManualOverride,
                    ticket,
                    ..pending
                });
            }
            None => state.pending = None,
        }
    }

    pub(crate) fn switch_interval_remaining(
        &self,
        now: Instant,
        min_interval: Duration,
    ) -> Duration {
        let nanos = self.last_switch_at_nanos.load(Ordering::Acquire);
        let since = if nanos == Self::NO_SWITCH {
            self.reference_instant
        } else {
            self.reference_instant + Duration::from_nanos(nanos)
        };
        min_interval.saturating_sub(now.duration_since(since))
    }

    pub fn unlock(&self) {
        let prev = self.lock_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "unlock called without matching lock");
    }

    delegate::delegate! {
        to self {
            /// Whether the anti-oscillation interval has elapsed. Before the first
            /// switch it is measured from the start of the session: that is when the
            /// throughput estimate rests on the fewest samples, so a fast first segment
            /// must not be enough to flip the variant out from under a listener who has
            /// barely started playing.
            #[expr($.is_zero())]
            #[call(switch_interval_remaining)]
            pub(super) fn can_switch_now(&self, now: Instant, min_interval: Duration) -> bool;
            /// Read-only peek at the pending decision. Returns the
            /// [`AbrDecision`] that [`apply_decision`](Self::apply_decision)
            /// would publish, or `None` when:
            /// - pending slot is empty;
            /// - state is locked (the seek-no-switch / blender invariant);
            /// - pending target equals `current` (no-op switch).
            ///
            /// Does not mutate. `current` is supplied by the caller to avoid a
            /// race with concurrent reads of [`current_variant_index`]; pass
            /// `self.current_variant_index()` if you do not need an externally
            /// pinned snapshot.
            #[must_use]
            #[expr($.map(PendingAbrDecision::decision))]
            #[call(claim_pending_decision)]
            pub fn peek_pending_decision(&self, current: VariantIndex) -> Option<AbrDecision>;
        }
    }
}
