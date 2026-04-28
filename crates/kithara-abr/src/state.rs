use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

#[cfg(any(test, feature = "internal"))]
use kithara_events::VariantDuration;
use kithara_events::{AbrMode, AbrReason, AbrVariant};
use kithara_platform::{Mutex, time::Instant};

use crate::controller::AbrSettings;

/// Outcome of an ABR decision step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbrDecision {
    pub target_variant_index: usize,
    pub reason: AbrReason,
    pub changed: bool,
}

/// Per-peer ABR state owned by a peer and shared with the controller.
pub struct AbrState {
    variants: Mutex<Vec<AbrVariant>>,
    current_variant: Arc<AtomicUsize>,
    mode: AtomicUsize,
    lock_count: AtomicUsize,
    max_bandwidth_bps: AtomicU64,
    last_switch_at_nanos: AtomicU64,
    reference_instant: Instant,
}

/// Snapshot of the inputs an [`AbrState`] needs to make a decision.
pub struct AbrView<'a> {
    pub estimate_bps: Option<u64>,
    pub buffer_ahead: Option<std::time::Duration>,
    pub bytes_downloaded: u64,
    pub variants: &'a [AbrVariant],
    pub settings: &'a AbrSettings,
}

/// Errors surfaced by ABR state mutations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AbrError {
    #[error("variant index {requested} out of bounds (available: {available})")]
    VariantOutOfBounds { requested: usize, available: usize },
}

impl AbrState {
    const NO_SWITCH: u64 = 0;
    const NO_BANDWIDTH_CAP: u64 = 0;

    /// Build an `AbrState` with the initial variant set from `mode`.
    #[must_use]
    pub fn new(variants: Vec<AbrVariant>, mode: AbrMode) -> Self {
        let initial_variant = match mode {
            AbrMode::Auto(Some(idx)) | AbrMode::Manual(idx) => idx,
            AbrMode::Auto(None) => 0,
        };
        Self {
            variants: Mutex::new(variants),
            current_variant: Arc::new(AtomicUsize::new(initial_variant)),
            mode: AtomicUsize::new(mode.into()),
            lock_count: AtomicUsize::new(0),
            max_bandwidth_bps: AtomicU64::new(Self::NO_BANDWIDTH_CAP),
            last_switch_at_nanos: AtomicU64::new(Self::NO_SWITCH),
            reference_instant: Instant::now(),
        }
    }

    #[must_use]
    pub fn current_variant_index(&self) -> usize {
        self.current_variant.load(Ordering::Acquire)
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

    #[must_use]
    pub fn mode(&self) -> AbrMode {
        AbrMode::from(self.mode.load(Ordering::Acquire))
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

    #[must_use]
    pub fn max_bandwidth_bps(&self) -> Option<u64> {
        let v = self.max_bandwidth_bps.load(Ordering::Acquire);
        if v == Self::NO_BANDWIDTH_CAP {
            None
        } else {
            Some(v)
        }
    }

    pub fn set_max_bandwidth_bps(&self, cap: Option<u64>) {
        self.max_bandwidth_bps
            .store(cap.unwrap_or(Self::NO_BANDWIDTH_CAP), Ordering::Release);
    }

    pub fn lock(&self) {
        self.lock_count.fetch_add(1, Ordering::AcqRel);
    }

    pub fn unlock(&self) {
        let prev = self.lock_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "unlock called without matching lock");
    }

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.lock_count.load(Ordering::Acquire) > 0
    }

    #[must_use]
    pub fn lock_count(&self) -> usize {
        self.lock_count.load(Ordering::Acquire)
    }

    /// Take a snapshot of the current variant list (cloned).
    #[must_use]
    pub fn variants_snapshot(&self) -> Vec<AbrVariant> {
        self.variants.lock_sync().clone()
    }

    /// Replace the variant list. Used at setup; not a hot path.
    pub fn set_variants(&self, variants: Vec<AbrVariant>) {
        *self.variants.lock_sync() = variants;
    }

    /// Produce a decision without mutating state.
    #[must_use]
    pub fn decide(&self, view: &AbrView<'_>, now: Instant) -> AbrDecision {
        let current = self.current_variant.load(Ordering::Acquire);

        // Gate 1: locked (seek in progress)
        if self.is_locked() {
            return decision(current, current, AbrReason::Locked);
        }

        // Gate 2: manual override
        if let AbrMode::Manual(idx) = self.mode() {
            return decision(current, idx, AbrReason::ManualOverride);
        }

        // Gate 3: min-switch interval
        if !self.can_switch_now(now, view.settings.min_switch_interval) {
            return decision(current, current, AbrReason::MinInterval);
        }

        // Gate 4: warmup
        if view.bytes_downloaded < view.settings.warmup_min_bytes {
            return decision(current, current, AbrReason::Warmup);
        }

        // Gate 5: no estimate yet
        let Some(estimate_bps) = view.estimate_bps else {
            return decision(current, current, AbrReason::NoEstimate);
        };

        let max_bw = self.max_bandwidth_bps();
        let sorted = sorted_candidates(view.variants, max_bw);
        if sorted.is_empty() {
            return decision(current, current, AbrReason::AlreadyOptimal);
        }

        let current_bw = current_bandwidth(&sorted, current);
        let adjusted_bps =
            adjusted_throughput(estimate_bps, view.settings.throughput_safety_factor);

        let Some((candidate_idx, candidate_bw)) = candidate_variant(&sorted, adjusted_bps) else {
            return decision(current, current, AbrReason::AlreadyOptimal);
        };

        // Forced down-switch when current exceeds cap.
        if let Some(cap) = max_bw
            && current_bw > cap
            && candidate_idx != current
        {
            return decision(current, candidate_idx, AbrReason::DownSwitch);
        }

        let ctx = SwitchContext {
            adjusted_bps,
            buffer_ahead: view.buffer_ahead,
            candidate_bw,
            candidate_idx,
            current,
            current_bw,
            settings: view.settings,
        };

        if candidate_bw > current_bw {
            return up_switch(ctx);
        }
        if candidate_bw < current_bw
            && let Some(d) = down_switch(ctx)
        {
            return d;
        }

        decision(current, current, AbrReason::AlreadyOptimal)
    }

    /// Commit a decision — record the new variant and the switch timestamp.
    ///
    /// Two legitimate callers write through this entry point:
    /// 1. [`AbrController::tick`](crate::AbrController) when the controller
    ///    decides on a new variant via the auto-mode FSM;
    /// 2. `kithara-hls` scheduler when the user manually selects a variant —
    ///    HLS holds `Arc<AbrState>` and applies a `Manual` decision so the
    ///    layout switch and the ABR state stay in sync.
    ///
    /// External crates that need to mutate `current_variant` for any other
    /// reason should be reviewed against the `redundant_accessors` lint —
    /// adding a third write-path is the architectural smell the lint catches.
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

    fn record_switch(&self, now: Instant) {
        self.last_switch_at_nanos
            .store(self.instant_to_nanos(now), Ordering::Release);
    }

    fn can_switch_now(&self, now: Instant, min_interval: std::time::Duration) -> bool {
        let nanos = self.last_switch_at_nanos.load(Ordering::Acquire);
        if nanos == Self::NO_SWITCH {
            return true;
        }
        let last = self.reference_instant + std::time::Duration::from_nanos(nanos);
        now.duration_since(last) >= min_interval
    }

    #[expect(clippy::cast_possible_truncation)]
    fn instant_to_nanos(&self, instant: Instant) -> u64 {
        let nanos = instant
            .saturating_duration_since(self.reference_instant)
            .as_nanos() as u64;
        nanos.max(1)
    }
}

// ───── free functions ─────

fn decision(current: usize, target: usize, reason: AbrReason) -> AbrDecision {
    AbrDecision {
        target_variant_index: target,
        reason,
        changed: target != current,
    }
}

fn sorted_candidates(variants: &[AbrVariant], max_bw: Option<u64>) -> Vec<(usize, u64)> {
    let mut out: Vec<(usize, u64)> = variants
        .iter()
        .filter(|v| max_bw.is_none_or(|cap| v.bandwidth_bps <= cap))
        .map(|v| (v.variant_index, v.bandwidth_bps))
        .collect();
    out.sort_by_key(|(_, bw)| *bw);
    out
}

fn current_bandwidth(sorted: &[(usize, u64)], current: usize) -> u64 {
    sorted
        .iter()
        .find(|(idx, _)| *idx == current)
        .map_or(0, |(_, bw)| *bw)
}

fn adjusted_throughput(estimate_bps: u64, safety_factor: f64) -> f64 {
    #[expect(clippy::cast_precision_loss)]
    let raw = estimate_bps as f64;
    (raw / safety_factor).max(0.0)
}

fn candidate_variant(sorted: &[(usize, u64)], adjusted_bps: f64) -> Option<(usize, u64)> {
    #[expect(clippy::cast_precision_loss)]
    let best_under = sorted
        .iter()
        .filter(|(_, bw)| (*bw as f64) <= adjusted_bps)
        .max_by_key(|(_, bw)| *bw);
    best_under
        .or_else(|| sorted.first())
        .map(|(idx, bw)| (*idx, *bw))
}

#[derive(Clone, Copy)]
struct SwitchContext<'a> {
    adjusted_bps: f64,
    buffer_ahead: Option<std::time::Duration>,
    candidate_bw: u64,
    candidate_idx: usize,
    current: usize,
    current_bw: u64,
    settings: &'a AbrSettings,
}

fn up_switch(ctx: SwitchContext<'_>) -> AbrDecision {
    let buffer_ok = ctx
        .buffer_ahead
        .is_none_or(|b| b >= ctx.settings.min_buffer_for_up_switch);
    #[expect(clippy::cast_precision_loss)]
    let headroom_ok =
        ctx.adjusted_bps >= (ctx.candidate_bw as f64) * ctx.settings.up_hysteresis_ratio;
    if buffer_ok && headroom_ok {
        return decision(ctx.current, ctx.candidate_idx, AbrReason::UpSwitch);
    }
    decision(ctx.current, ctx.current, AbrReason::BufferTooLowForUpSwitch)
}

fn down_switch(ctx: SwitchContext<'_>) -> Option<AbrDecision> {
    let urgent = ctx
        .buffer_ahead
        .is_some_and(|b| b <= ctx.settings.urgent_downswitch_buffer);
    #[expect(clippy::cast_precision_loss)]
    let margin_ok =
        ctx.adjusted_bps <= (ctx.current_bw as f64) * ctx.settings.down_hysteresis_ratio;
    if urgent {
        return Some(decision(
            ctx.current,
            ctx.candidate_idx,
            AbrReason::UrgentDownSwitch,
        ));
    }
    if margin_ok {
        return Some(decision(
            ctx.current,
            ctx.candidate_idx,
            AbrReason::DownSwitch,
        ));
    }
    None
}

/// Build a canonical set of 3 variants for tests and fixtures.
#[cfg(any(test, feature = "internal"))]
#[must_use]
pub fn test_variants_3() -> Vec<AbrVariant> {
    vec![
        AbrVariant {
            variant_index: 0,
            bandwidth_bps: 256_000,
            duration: VariantDuration::Unknown,
        },
        AbrVariant {
            variant_index: 1,
            bandwidth_bps: 512_000,
            duration: VariantDuration::Unknown,
        },
        AbrVariant {
            variant_index: 2,
            bandwidth_bps: 1_024_000,
            duration: VariantDuration::Unknown,
        },
    ]
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::controller::AbrSettings;

    fn settings_fast() -> AbrSettings {
        AbrSettings {
            warmup_min_bytes: 0,
            min_switch_interval: Duration::ZERO,
            min_buffer_for_up_switch: Duration::ZERO,
            ..AbrSettings::default()
        }
    }

    fn view_with_bw<'a>(
        bps: Option<u64>,
        variants: &'a [AbrVariant],
        settings: &'a AbrSettings,
    ) -> AbrView<'a> {
        AbrView {
            estimate_bps: bps,
            buffer_ahead: None,
            bytes_downloaded: 10 * 1024 * 1024,
            variants,
            settings,
        }
    }

    #[kithara::test]
    fn decide_locked_never_switches() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        state.lock();
        let variants = test_variants_3();
        let settings = settings_fast();
        let view = view_with_bw(Some(10_000_000), &variants, &settings);
        let d = state.decide(&view, Instant::now());
        assert!(!d.changed);
        assert_eq!(d.reason, AbrReason::Locked);
    }

    #[kithara::test]
    fn decide_many_samples_during_lock_never_switches() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        let initial = state.current_variant_index();
        state.lock();
        let variants = test_variants_3();
        let settings = settings_fast();
        for i in 0..100u64 {
            let view = view_with_bw(Some(10_000_000 * (i + 1)), &variants, &settings);
            let _ = state.decide(&view, Instant::now() + Duration::from_secs(i * 60));
        }
        assert_eq!(state.current_variant_index(), initial);
    }

    #[kithara::test]
    fn decide_warmup_blocks_switch() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        let variants = test_variants_3();
        let settings = AbrSettings {
            warmup_min_bytes: 128 * 1024,
            min_switch_interval: Duration::ZERO,
            min_buffer_for_up_switch: Duration::ZERO,
            ..AbrSettings::default()
        };
        let view = AbrView {
            estimate_bps: Some(10_000_000),
            buffer_ahead: None,
            bytes_downloaded: 1024,
            variants: &variants,
            settings: &settings,
        };
        let d = state.decide(&view, Instant::now());
        assert_eq!(d.reason, AbrReason::Warmup);
        assert!(!d.changed);
    }

    #[kithara::test]
    fn decide_manual_mode_always_target() {
        let state = AbrState::new(test_variants_3(), AbrMode::Manual(2));
        let variants = test_variants_3();
        let settings = settings_fast();
        let view = view_with_bw(None, &variants, &settings);
        let d = state.decide(&view, Instant::now());
        assert_eq!(d.reason, AbrReason::ManualOverride);
        assert_eq!(d.target_variant_index, 2);
    }

    #[kithara::test]
    fn decide_no_estimate_stays_put() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(1)));
        let variants = test_variants_3();
        let settings = settings_fast();
        let view = view_with_bw(None, &variants, &settings);
        let d = state.decide(&view, Instant::now());
        assert_eq!(d.reason, AbrReason::NoEstimate);
        assert!(!d.changed);
    }

    #[kithara::test]
    fn decide_upswitch_when_bandwidth_allows() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        let variants = test_variants_3();
        let settings = settings_fast();
        let view = view_with_bw(Some(3_000_000), &variants, &settings);
        let d = state.decide(&view, Instant::now());
        assert_eq!(d.reason, AbrReason::UpSwitch);
        assert_eq!(d.target_variant_index, 2);
    }

    #[kithara::test]
    fn decide_downswitch_when_bandwidth_drops() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(2)));
        let variants = test_variants_3();
        let settings = settings_fast();
        let view = view_with_bw(Some(300_000), &variants, &settings);
        let d = state.decide(&view, Instant::now());
        assert_eq!(d.reason, AbrReason::DownSwitch);
        assert_eq!(d.target_variant_index, 0);
    }

    #[kithara::test]
    fn decide_urgent_downswitch_when_buffer_low() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(2)));
        let variants = test_variants_3();
        let settings = AbrSettings {
            urgent_downswitch_buffer: Duration::from_secs(5),
            down_hysteresis_ratio: 0.01, // high threshold — only urgent path fires
            min_switch_interval: Duration::ZERO,
            warmup_min_bytes: 0,
            ..AbrSettings::default()
        };
        let view = AbrView {
            estimate_bps: Some(700_000),
            buffer_ahead: Some(Duration::from_secs(2)),
            bytes_downloaded: 10 * 1024 * 1024,
            variants: &variants,
            settings: &settings,
        };
        let d = state.decide(&view, Instant::now());
        assert_eq!(d.reason, AbrReason::UrgentDownSwitch);
    }

    #[kithara::test]
    fn decide_buffer_too_low_for_upswitch() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        let variants = test_variants_3();
        let settings = AbrSettings {
            min_buffer_for_up_switch: Duration::from_secs(10),
            min_switch_interval: Duration::ZERO,
            warmup_min_bytes: 0,
            ..AbrSettings::default()
        };
        let view = AbrView {
            estimate_bps: Some(3_000_000),
            buffer_ahead: Some(Duration::from_secs(2)),
            bytes_downloaded: 10 * 1024 * 1024,
            variants: &variants,
            settings: &settings,
        };
        let d = state.decide(&view, Instant::now());
        assert_eq!(d.reason, AbrReason::BufferTooLowForUpSwitch);
        assert!(!d.changed);
    }

    #[kithara::test]
    fn apply_updates_current_variant_and_timestamp() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        state.apply(
            &AbrDecision {
                target_variant_index: 2,
                reason: AbrReason::UpSwitch,
                changed: true,
            },
            Instant::now(),
        );
        assert_eq!(state.current_variant_index(), 2);
    }

    #[kithara::test]
    fn apply_noop_when_same_variant() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(1)));
        state.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::AlreadyOptimal,
                changed: false,
            },
            Instant::now(),
        );
        assert_eq!(state.current_variant_index(), 1);
    }

    #[kithara::test]
    fn set_mode_rejects_out_of_bounds_manual() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        let err = state.set_mode(AbrMode::Manual(10)).unwrap_err();
        assert_eq!(
            err,
            AbrError::VariantOutOfBounds {
                requested: 10,
                available: 3,
            }
        );
    }

    #[kithara::test]
    fn lock_is_refcounted() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(None));
        state.lock();
        state.lock();
        assert!(state.is_locked());
        state.unlock();
        assert!(state.is_locked());
        state.unlock();
        assert!(!state.is_locked());
    }

    #[kithara::test]
    fn min_switch_interval_prevents_oscillation() {
        let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
        let variants = test_variants_3();
        let settings = AbrSettings {
            min_switch_interval: Duration::from_secs(30),
            warmup_min_bytes: 0,
            min_buffer_for_up_switch: Duration::ZERO,
            ..AbrSettings::default()
        };
        let now = Instant::now();
        let view = AbrView {
            estimate_bps: Some(3_000_000),
            buffer_ahead: None,
            bytes_downloaded: 10 * 1024 * 1024,
            variants: &variants,
            settings: &settings,
        };
        let d1 = state.decide(&view, now);
        assert!(d1.changed);
        state.apply(&d1, now);
        let d2 = state.decide(&view, now + Duration::from_secs(1));
        assert_eq!(d2.reason, AbrReason::MinInterval);
    }
}
