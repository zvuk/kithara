use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use kithara_events::{AbrMode, AbrVariant};
use kithara_platform::{Mutex, time::Instant};
use num_traits::ToPrimitive;

use super::{decision::AbrDecision, error::AbrError, view::AbrView};

/// Per-peer ABR state owned by a peer and shared with the controller.
pub struct AbrState {
    current_variant: Arc<AtomicUsize>,
    last_switch_at_nanos: AtomicU64,
    max_bandwidth_bps: AtomicU64,
    lock_count: AtomicUsize,
    mode: AtomicUsize,
    reference_instant: Instant,
    variants: Mutex<Vec<AbrVariant>>,
}

impl AbrState {
    const NO_BANDWIDTH_CAP: u64 = 0;
    const NO_SWITCH: u64 = 0;

    /// Build an `AbrState` with the initial variant set from `mode`.
    #[must_use]
    pub fn new(variants: Vec<AbrVariant>, mode: AbrMode) -> Self {
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
            variants: Mutex::new(variants),
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
    pub fn apply(&self, d: &AbrDecision, now: Instant) {
        if cfg!(debug_assertions) {
            let variants = self.variants.lock_sync();
            // Only enforce membership once variants have been populated.
            // Early-init scenarios (peer created before master playlist parse)
            // legitimately apply a variant while the list is still empty.
            if !variants.is_empty() {
                let ok = variants
                    .iter()
                    .any(|v| v.variant_index == d.target_variant_index);
                let available: Vec<usize> = variants.iter().map(|v| v.variant_index).collect();
                drop(variants);
                debug_assert!(
                    ok,
                    "ABR decision references nonexistent variant {} (available: {:?})",
                    d.target_variant_index, available
                );
            }
        }
        let current = self.current_variant.load(Ordering::Acquire);
        if d.target_variant_index == current {
            return;
        }
        self.current_variant
            .store(d.target_variant_index, Ordering::Release);
        self.record_switch(now);
    }

    pub(super) fn can_switch_now(&self, now: Instant, min_interval: std::time::Duration) -> bool {
        let nanos = self.last_switch_at_nanos.load(Ordering::Acquire);
        if nanos == Self::NO_SWITCH {
            return true;
        }
        let last = self.reference_instant + std::time::Duration::from_nanos(nanos);
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

    pub fn set_max_bandwidth_bps(&self, cap: Option<u64>) {
        self.max_bandwidth_bps
            .store(cap.unwrap_or(Self::NO_BANDWIDTH_CAP), Ordering::Release);
    }

    /// Apply a new mode.
    ///
    /// # Errors
    /// Returns [`AbrError::VariantOutOfBounds`] if `mode` is
    /// `AbrMode::Manual(idx)` and `idx` is not a known variant index.
    pub fn set_mode(&self, mode: AbrMode) -> Result<(), AbrError> {
        if let AbrMode::Manual(idx) = mode {
            let variants = self.variants.lock_sync();
            if !variants.iter().any(|v| v.variant_index == idx) {
                return Err(AbrError::VariantOutOfBounds {
                    requested: idx,
                    available: variants.len(),
                });
            }
        }
        self.mode.store(mode.into(), Ordering::Release);
        Ok(())
    }

    /// Force a variant index without going through `apply()` / the controller.
    ///
    /// Used in `kithara-hls` integration tests to set up scenarios that would
    /// otherwise require a full ABR tick cycle to reach. Gated under the
    /// `internal` feature so production callers cannot bypass the FSM.
    #[cfg(any(test, feature = "internal"))]
    pub fn set_variant_for_test(&self, idx: usize) {
        self.current_variant.store(idx, Ordering::Release);
    }

    /// Replace the variant list. Used at setup; not a hot path.
    pub fn set_variants(&self, variants: Vec<AbrVariant>) {
        *self.variants.lock_sync() = variants;
    }

    pub fn unlock(&self) {
        let prev = self.lock_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "unlock called without matching lock");
    }

    /// Take a snapshot of the current variant list (cloned).
    #[must_use]
    pub fn variants_snapshot(&self) -> Vec<AbrVariant> {
        self.variants.lock_sync().clone()
    }
}
