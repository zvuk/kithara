use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use kithara_events::{AbrMode, AbrReason};
use kithara_platform::{
    Mutex,
    time::{Duration, Instant},
};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{decision::AbrDecision, view::AbrView};

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
    lock_count: AtomicUsize,
    mode: AtomicUsize,
    reference_instant: Instant,
    /// Phase 2 boundary-commit slot. `Some` means a switch has been
    /// requested via [`request_target`](AbrState::request_target) and
    /// the scheduler has not yet observed it on a segment boundary.
    /// Replace-pending semantics: a fresh `request_target` overwrites
    /// any prior unobserved entry (latest-wins, matching the
    /// "switch-only-on-boundaries" contract from the two-cursor plan).
    pending: Mutex<Option<PendingApply>>,
}

/// Captured intent of a pending switch: the target variant index plus
/// the reason the requestor (controller, manual UI, scheduler) wants
/// recorded once the boundary commit lands.
#[derive(Clone, Copy, Debug)]
struct PendingApply {
    target: usize,
    reason: AbrReason,
}

impl AbrState {
    const NO_BANDWIDTH_CAP: u64 = 0;
    const NO_SWITCH: u64 = 0;

    /// Build an `AbrState` with the initial variant set from `mode`.
    #[must_use]
    pub fn new(mode: AbrMode) -> Self {
        let initial_variant = match mode {
            AbrMode::Auto(Some(idx)) | AbrMode::Manual(idx) => idx,
            AbrMode::Auto(None) => 0,
        };
        Self {
            current_variant: Arc::new(AtomicUsize::new(initial_variant)),
            last_switch_at_nanos: AtomicU64::new(Self::NO_SWITCH),
            max_bandwidth_bps: AtomicU64::new(Self::NO_BANDWIDTH_CAP),
            lock_count: AtomicUsize::new(0),
            mode: AtomicUsize::new(mode.into()),
            reference_instant: Instant::now(),
            pending: Mutex::new(None),
        }
    }

    /// Commit a decision — record the new variant and the switch timestamp.
    ///
    /// Two legitimate callers write through this entry point:
    /// 1. [`AbrController::tick`](crate::AbrController) when the controller
    ///    decides on a new variant via the auto-mode FSM;
    /// 2. `kithara-hls` scheduler when the user manually selects a variant —
    ///    HLS holds `Arc<AbrState>` and applies a `Manual` decision so the
    ///    layout switch and the ABR state stay in sync.
    #[kithara::probe(d)]
    pub fn apply(&self, d: &AbrDecision, now: Instant) {
        let current = self.current_variant.load(Ordering::Acquire);
        if d.target_variant_index == current {
            return;
        }
        self.current_variant
            .store(d.target_variant_index, Ordering::Release);
        self.record_switch(now);
    }

    pub(super) fn can_switch_now(&self, now: Instant, min_interval: Duration) -> bool {
        let nanos = self.last_switch_at_nanos.load(Ordering::Acquire);
        if nanos == Self::NO_SWITCH {
            return true;
        }
        let last = self.reference_instant + Duration::from_nanos(nanos);
        now.duration_since(last) >= min_interval
    }

    #[must_use]
    pub fn current_variant_index(&self) -> usize {
        self.current_variant.load(Ordering::Acquire)
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

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.lock_count.load(Ordering::Acquire) > 0
    }

    pub fn lock(&self) {
        self.lock_count.fetch_add(1, Ordering::AcqRel);
    }

    #[must_use]
    pub fn lock_count(&self) -> usize {
        self.lock_count.load(Ordering::Acquire)
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

    fn record_switch(&self, now: Instant) {
        self.last_switch_at_nanos
            .store(self.instant_to_nanos(now), Ordering::Release);
    }

    /// Phase 2 of the two-cursor refactor: record the intent to switch
    /// to `target` without committing the variant change. The boundary
    /// commit is driven by [`commit_pending`](Self::commit_pending) at
    /// segment boundaries (Phase 3 wires the scheduler to call it).
    ///
    /// Replace-pending semantics: a fresh `request_target` overwrites
    /// any prior unobserved entry. This honours the user-stated
    /// contract that a switch only commits at the next segment boundary
    /// — between two boundaries we keep the latest intent and discard
    /// stale ones.
    pub fn request_target(&self, target: usize, reason: AbrReason) {
        *self.pending.lock_sync() = Some(PendingApply { target, reason });
    }

    /// Phase 2 read-only view of the unobserved pending switch (if any).
    /// Used by the Phase 3 scheduler boundary check and by tests.
    #[must_use]
    pub fn pending_target(&self) -> Option<usize> {
        self.pending.lock_sync().as_ref().map(|p| p.target)
    }

    /// Phase 2 boundary commit: if a switch is pending and ABR is not
    /// locked (the blender-fence / seek-no-switch invariant), atomically
    /// move `current_variant` to the pending target, record the switch
    /// timestamp, and return the resulting [`AbrDecision`]. Returns
    /// `None` when no pending intent exists, when ABR is locked, or
    /// when the pending target equals `current_variant` (no-op switch).
    ///
    /// Phase 3: the scheduler calls this at each detected segment
    /// boundary; the gating on [`is_locked`](Self::is_locked) prevents
    /// a chain switch from committing while the blender period (held
    /// open by `lock`) is still draining the previous `V_old` → `V_new`
    /// crossfade.
    #[must_use]
    pub fn commit_pending(&self, now: Instant) -> Option<AbrDecision> {
        if self.is_locked() {
            return None;
        }
        let pending = self.pending.lock_sync().take()?;
        let current = self.current_variant.load(Ordering::Acquire);
        if pending.target == current {
            return None;
        }
        self.current_variant
            .store(pending.target, Ordering::Release);
        self.record_switch(now);
        Some(AbrDecision {
            target_variant_index: pending.target,
            reason: pending.reason,
            did_change: true,
        })
    }

    pub fn set_max_bandwidth_bps(&self, cap: Option<u64>) {
        self.max_bandwidth_bps
            .store(cap.unwrap_or(Self::NO_BANDWIDTH_CAP), Ordering::Release);
    }

    /// Apply a new mode. Caller is responsible for validating that
    /// `Manual(idx)` references a known variant — variants live on the
    /// peer, not the state, so validation must happen against
    /// [`Abr::variants()`](crate::Abr::variants) at the call site.
    pub fn set_mode(&self, mode: AbrMode) {
        self.mode.store(mode.into(), Ordering::Release);
    }

    pub fn unlock(&self) {
        let prev = self.lock_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "unlock called without matching lock");
    }
}
