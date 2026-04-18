use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use kithara_platform::{Mutex, time::Instant};

use super::{AbrMode, AbrOptions, ThroughputEstimator, ThroughputSample, Variant};
use crate::estimator::Estimator;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbrReason {
    Initial,
    ManualOverride,
    UpSwitch,
    DownSwitch,
    MinInterval,
    NoEstimate,
    BufferTooLowForUpSwitch,
    AlreadyOptimal,
    Locked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbrDecision {
    pub target_variant_index: usize,
    pub reason: AbrReason,
    pub changed: bool,
}

/// Adaptive bitrate controller.
///
/// Clone-able: cloning shares the same internal state (all fields behind Arc).
/// This allows sharing one controller between multiple players or holding a
/// clone at the Resource level for runtime mode changes.

#[derive(Clone)]
pub struct AbrController<E: Estimator> {
    cfg: Arc<AbrOptions>,
    variants: Arc<Mutex<Vec<Variant>>>,
    mode: Arc<AtomicUsize>,
    max_bandwidth_bps: Arc<AtomicU64>,
    current_variant: Arc<AtomicUsize>,
    lock_count: Arc<AtomicUsize>,
    estimator: Arc<Mutex<E>>,
    last_switch_at_nanos: Arc<AtomicU64>,
    reference_instant: Instant,
}

#[derive(Clone, Copy)]
struct SwitchContext {
    adjusted_bps: f64,
    buffer_level_secs: f64,
    candidate_bw: u64,
    candidate_idx: usize,
    current: usize,
    current_bw: u64,
}

impl<E: Estimator> AbrController<E> {
    const NO_SWITCH: u64 = 0;
    const NO_BANDWIDTH_CAP: u64 = 0;

    pub fn with_estimator(cfg: AbrOptions, estimator: E) -> Self {
        let initial_variant = cfg.initial_variant();
        let mode_val: usize = cfg.mode.into();
        let variants = cfg.variants.clone();
        let max_bw = cfg.max_bandwidth_bps.unwrap_or(Self::NO_BANDWIDTH_CAP);
        Self {
            variants: Arc::new(Mutex::new(variants)),
            max_bandwidth_bps: Arc::new(AtomicU64::new(max_bw)),
            cfg: Arc::new(cfg),
            mode: Arc::new(AtomicUsize::new(mode_val)),
            current_variant: Arc::new(AtomicUsize::new(initial_variant)),
            lock_count: Arc::new(AtomicUsize::new(0)),
            estimator: Arc::new(Mutex::new(estimator)),
            last_switch_at_nanos: Arc::new(AtomicU64::new(Self::NO_SWITCH)),
            reference_instant: Instant::now(),
        }
    }

    /// Set available variants (called after playlist discovery).
    pub fn set_variants(&self, variants: Vec<Variant>) {
        *self.variants.lock_sync() = variants;
    }

    delegate::delegate! {
        to self.estimator.lock_sync() {
            pub fn push_sample(&self, sample: ThroughputSample);
            pub fn reset_buffer(&self);
            #[must_use]
            pub fn buffer_level_secs(&self) -> f64;
        }
    }

    #[must_use]
    pub fn get_current_variant_index(&self) -> usize {
        self.current_variant.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn variant_index_handle(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.current_variant)
    }

    #[must_use]
    pub fn min_throughput_record_ms(&self) -> u128 {
        self.cfg.min_throughput_record_ms
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

    #[must_use]
    pub fn mode(&self) -> AbrMode {
        AbrMode::from(self.mode.load(Ordering::Acquire))
    }

    pub fn set_mode(&self, mode: AbrMode) {
        self.mode.store(mode.into(), Ordering::Release);
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

    pub fn decide(&self, now: Instant) -> AbrDecision {
        let current = self.current_variant.load(Ordering::Acquire);

        if self.is_locked() {
            return Self::decision(current, current, AbrReason::Locked);
        }

        if let AbrMode::Manual(idx) = self.mode() {
            return Self::decision(current, idx, AbrReason::ManualOverride);
        }

        let buffer_level_secs = self.buffer_level_secs();

        if !self.can_switch_now(now) {
            tracing::debug!(
                current,
                buffer_level_secs,
                "ABR decide: MinInterval not elapsed"
            );
            return Self::decision(current, current, AbrReason::MinInterval);
        }

        let Some(estimate_bps) = self.estimator.lock_sync().estimate_bps() else {
            tracing::debug!(current, buffer_level_secs, "ABR decide: NoEstimate");
            return Self::decision(current, current, AbrReason::NoEstimate);
        };

        let current_bw = self.current_variant_bandwidth(current);
        let variants = self.variants_sorted_by_bandwidth();
        if variants.is_empty() {
            return Self::decision(current, current, AbrReason::AlreadyOptimal);
        }

        let adjusted_bps = self.adjusted_throughput_bps(estimate_bps);

        tracing::debug!(
            current,
            current_bw,
            estimate_bps,
            adjusted_bps,
            buffer_level_secs,
            safety_factor = self.cfg.throughput_safety_factor,
            min_buffer_for_up = self.cfg.min_buffer_for_up_switch_secs,
            up_hysteresis = self.cfg.up_hysteresis_ratio,
            variants_count = variants.len(),
            "ABR decide: evaluating"
        );

        let Some((candidate_idx, candidate_bw)) = Self::candidate_variant(&variants, adjusted_bps)
        else {
            return Self::decision(current, current, AbrReason::AlreadyOptimal);
        };

        tracing::debug!(
            candidate_idx,
            candidate_bw,
            current,
            current_bw,
            "ABR decide: candidate"
        );

        if let Some(cap) = self.max_bandwidth_bps()
            && current_bw > cap
            && candidate_idx != current
        {
            self.record_switch(now);
            return Self::decision(current, candidate_idx, AbrReason::DownSwitch);
        }

        let ctx = SwitchContext {
            adjusted_bps,
            buffer_level_secs,
            candidate_bw,
            candidate_idx,
            current,
            current_bw,
        };

        if let Some(d) = self.maybe_up_switch(now, ctx) {
            return d;
        }
        if let Some(d) = self.maybe_down_switch(now, ctx) {
            return d;
        }

        Self::decision(current, current, AbrReason::AlreadyOptimal)
    }

    pub fn apply(&self, decision: &AbrDecision, now: Instant) {
        let current = self.current_variant.load(Ordering::Acquire);
        if decision.target_variant_index == current {
            return;
        }
        self.current_variant
            .store(decision.target_variant_index, Ordering::Release);
        self.record_switch(now);
    }

    fn decision(current: usize, target: usize, reason: AbrReason) -> AbrDecision {
        AbrDecision {
            target_variant_index: target,
            reason,
            changed: target != current,
        }
    }

    #[expect(clippy::cast_possible_truncation)]
    fn instant_to_nanos(&self, instant: Instant) -> u64 {
        let nanos = instant
            .saturating_duration_since(self.reference_instant)
            .as_nanos() as u64;
        nanos.max(1)
    }

    fn record_switch(&self, now: Instant) {
        self.last_switch_at_nanos
            .store(self.instant_to_nanos(now), Ordering::Release);
    }

    fn can_switch_now(&self, now: Instant) -> bool {
        let nanos = self.last_switch_at_nanos.load(Ordering::Acquire);
        if nanos == Self::NO_SWITCH {
            return true;
        }
        let last = self.reference_instant + Duration::from_nanos(nanos);
        now.duration_since(last) >= self.cfg.min_switch_interval
    }

    fn current_variant_bandwidth(&self, current: usize) -> u64 {
        self.variants
            .lock_sync()
            .iter()
            .find(|v| v.variant_index == current)
            .map_or(0, |v| v.bandwidth_bps)
    }

    fn variants_sorted_by_bandwidth(&self) -> Vec<(usize, u64)> {
        let max_bw = self.max_bandwidth_bps();
        let mut sorted: Vec<(usize, u64)> = self
            .variants
            .lock_sync()
            .iter()
            .filter(|v| max_bw.is_none_or(|cap| v.bandwidth_bps <= cap))
            .map(|v| (v.variant_index, v.bandwidth_bps))
            .collect();
        sorted.sort_by_key(|(_, bw)| *bw);
        sorted
    }

    #[expect(clippy::cast_precision_loss)]
    fn adjusted_throughput_bps(&self, estimate_bps: u64) -> f64 {
        (estimate_bps as f64 / self.cfg.throughput_safety_factor).max(0.0)
    }

    fn candidate_variant(variants: &[(usize, u64)], adjusted_bps: f64) -> Option<(usize, u64)> {
        #[expect(clippy::cast_precision_loss)]
        let best_under = variants
            .iter()
            .filter(|(_, bw)| (*bw as f64) <= adjusted_bps)
            .max_by_key(|(_, bw)| *bw);
        best_under
            .or_else(|| variants.first())
            .map(|(idx, bw)| (*idx, *bw))
    }

    fn maybe_up_switch(&self, now: Instant, ctx: SwitchContext) -> Option<AbrDecision> {
        if ctx.candidate_bw <= ctx.current_bw {
            return None;
        }
        let buffer_ok = self.cfg.min_buffer_for_up_switch_secs <= 0.0
            || ctx.buffer_level_secs >= self.cfg.min_buffer_for_up_switch_secs;
        #[expect(clippy::cast_precision_loss)]
        let headroom_ok =
            ctx.adjusted_bps >= (ctx.candidate_bw as f64) * self.cfg.up_hysteresis_ratio;
        #[expect(clippy::cast_precision_loss)]
        let required_bps = (ctx.candidate_bw as f64) * self.cfg.up_hysteresis_ratio;
        tracing::debug!(
            buffer_ok,
            headroom_ok,
            buffer_level_secs = ctx.buffer_level_secs,
            min_buffer = self.cfg.min_buffer_for_up_switch_secs,
            adjusted_bps = ctx.adjusted_bps,
            required_bps,
            "ABR decide: up-switch check"
        );
        if buffer_ok && headroom_ok {
            self.record_switch(now);
            return Some(Self::decision(
                ctx.current,
                ctx.candidate_idx,
                AbrReason::UpSwitch,
            ));
        }
        Some(Self::decision(
            ctx.current,
            ctx.current,
            AbrReason::BufferTooLowForUpSwitch,
        ))
    }

    fn maybe_down_switch(&self, now: Instant, ctx: SwitchContext) -> Option<AbrDecision> {
        if ctx.candidate_bw >= ctx.current_bw {
            return None;
        }
        let urgent_down = ctx.buffer_level_secs <= self.cfg.down_switch_buffer_secs;
        #[expect(clippy::cast_precision_loss)]
        let margin_ok =
            ctx.adjusted_bps <= (ctx.current_bw as f64) * self.cfg.down_hysteresis_ratio;
        if urgent_down || margin_ok {
            self.record_switch(now);
            return Some(Self::decision(
                ctx.current,
                ctx.candidate_idx,
                AbrReason::DownSwitch,
            ));
        }
        None
    }
}

impl AbrController<ThroughputEstimator> {
    #[must_use]
    pub fn new(cfg: AbrOptions) -> Self {
        let estimator = ThroughputEstimator::new(&cfg);
        Self::with_estimator(cfg, estimator)
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::{estimator::EstimatorMock, types::Variant},
        *,
    };

    fn variants() -> Vec<Variant> {
        vec![
            Variant {
                variant_index: 0,
                bandwidth_bps: 256_000,
            },
            Variant {
                variant_index: 1,
                bandwidth_bps: 512_000,
            },
            Variant {
                variant_index: 2,
                bandwidth_bps: 1_024_000,
            },
        ]
    }

    fn estimator_static(bps: Option<u64>, buffer_secs: f64) -> Unimock {
        Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(bps),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(buffer_secs),
        ))
    }

    fn opts(mode: AbrMode) -> AbrOptions {
        AbrOptions {
            mode,
            variants: variants(),
            min_switch_interval: Duration::from_secs(0),
            ..AbrOptions::default()
        }
    }

    #[kithara::test]
    fn set_mode_auto_to_manual() {
        let ctrl = AbrController::with_estimator(
            opts(AbrMode::Auto(Some(0))),
            estimator_static(None, 0.0),
        );
        assert_eq!(ctrl.decide(Instant::now()).reason, AbrReason::NoEstimate);

        ctrl.set_mode(AbrMode::Manual(2));
        let d = ctrl.decide(Instant::now());
        assert_eq!(d.reason, AbrReason::ManualOverride);
        assert_eq!(d.target_variant_index, 2);
    }

    #[kithara::test]
    fn set_mode_manual_to_auto() {
        let ctrl =
            AbrController::with_estimator(opts(AbrMode::Manual(1)), estimator_static(None, 0.0));
        assert_eq!(
            ctrl.decide(Instant::now()).reason,
            AbrReason::ManualOverride
        );

        ctrl.set_mode(AbrMode::Auto(None));
        assert_eq!(ctrl.decide(Instant::now()).reason, AbrReason::NoEstimate);
    }

    #[kithara::test]
    fn clone_shares_state() {
        let ctrl = AbrController::with_estimator(opts(AbrMode::Auto(None)), Unimock::new(()));
        let clone = ctrl.clone();
        clone.set_mode(AbrMode::Manual(2));
        assert_eq!(ctrl.mode(), AbrMode::Manual(2));
    }

    #[kithara::test]
    fn clone_shares_variant() {
        let ctrl = AbrController::with_estimator(opts(AbrMode::Manual(1)), Unimock::new(()));
        let clone = ctrl.clone();
        clone.apply(
            &AbrDecision {
                target_variant_index: 2,
                reason: AbrReason::ManualOverride,
                changed: true,
            },
            Instant::now(),
        );
        assert_eq!(ctrl.get_current_variant_index(), 2);
    }

    #[kithara::test]
    fn lock_prevents_decide() {
        let ctrl =
            AbrController::with_estimator(opts(AbrMode::Auto(None)), estimator_static(None, 0.0));
        ctrl.lock();
        assert_eq!(ctrl.decide(Instant::now()).reason, AbrReason::Locked);
        ctrl.unlock();
        assert_ne!(ctrl.decide(Instant::now()).reason, AbrReason::Locked);
    }

    #[kithara::test]
    fn lock_is_ref_counted() {
        let ctrl = AbrController::with_estimator(opts(AbrMode::Auto(None)), Unimock::new(()));
        ctrl.lock();
        ctrl.lock();
        assert!(ctrl.is_locked());
        ctrl.unlock();
        assert!(ctrl.is_locked());
        ctrl.unlock();
        assert!(!ctrl.is_locked());
    }

    #[kithara::test]
    fn two_tracks_shared_controller_lock_unlock() {
        let opts = AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            variants: variants(),
            min_switch_interval: Duration::ZERO,
            min_buffer_for_up_switch_secs: 0.0,
            ..AbrOptions::default()
        };
        let ctrl = AbrController::new(opts);
        let track_a = ctrl.clone();
        let track_b = ctrl.clone();

        let sample = |bytes| ThroughputSample {
            bytes,
            duration: Duration::from_secs(1),
            at: Instant::now(),
            source: super::super::ThroughputSampleSource::Network,
            content_duration: None,
        };

        // Both tracks push throughput — ABR should want to upswitch.
        track_a.push_sample(sample(2_000_000));
        track_b.push_sample(sample(2_000_000));

        let d = ctrl.decide(Instant::now());
        assert!(d.changed, "should upswitch before lock");
        ctrl.apply(&d, Instant::now());
        let switched_variant = d.target_variant_index;
        assert_ne!(switched_variant, 0);

        // Track A starts a seek — locks ABR.
        track_a.lock();
        let far_future = Instant::now() + Duration::from_secs(120);
        let d = ctrl.decide(far_future);
        assert_eq!(
            d.reason,
            AbrReason::Locked,
            "locked controller must not switch"
        );
        assert!(!d.changed);

        // Track B also seeks — stacks a second lock.
        track_b.lock();
        let d = ctrl.decide(far_future);
        assert_eq!(d.reason, AbrReason::Locked);

        // Track A finishes seek — one lock remains.
        track_a.unlock();
        assert!(ctrl.is_locked(), "still locked by track B");
        let d = ctrl.decide(far_future);
        assert_eq!(d.reason, AbrReason::Locked);

        // Track B finishes seek — fully unlocked, ABR resumes.
        track_b.unlock();
        assert!(!ctrl.is_locked());
        let d = ctrl.decide(far_future);
        assert_ne!(
            d.reason,
            AbrReason::Locked,
            "ABR should resume after full unlock"
        );
    }

    #[kithara::test]
    fn set_variants_enables_decisions() {
        let ctrl = AbrController::new(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            min_switch_interval: Duration::ZERO,
            min_buffer_for_up_switch_secs: 0.0,
            ..AbrOptions::default()
        });

        // No variants → no meaningful decision
        ctrl.push_sample(ThroughputSample {
            bytes: 3_000_000 / 8,
            duration: Duration::from_secs(1),
            at: Instant::now(),
            source: super::super::ThroughputSampleSource::Network,
            content_duration: None,
        });
        let d = ctrl.decide(Instant::now());
        assert!(!d.changed, "no variants means no switch");

        // Inject variants at runtime
        ctrl.set_variants(variants());
        let d = ctrl.decide(Instant::now());
        assert!(d.changed, "with variants, ABR should switch");
        assert_eq!(d.target_variant_index, 2);
    }

    // Parametrized ABR decision test.
    // Columns: name, start_variant, throughput_bytes, buffer_secs,
    //          down_switch_buf, min_buf_up, safety_factor,
    //          expected_variant, expected_reason, expected_changed
    #[kithara::test]
    #[case("downswitch_low_throughput",  2, 300_000/8, 10.0, 0.0, 0.0, 1.5, 0, AbrReason::DownSwitch, true)]
    #[case("upswitch_high_throughput",   0, 2_000_000/8, 0.0, 0.0, 0.0, 1.5, 2, AbrReason::UpSwitch, true)]
    #[case(
        "downswitch_buffer_too_low",
        2,
        30_000,
        0.1,
        0.0,
        0.0,
        1.5,
        0,
        AbrReason::DownSwitch,
        true
    )]
    #[case("no_change_mid_variant",      1, 800_000/8, 10.0, 0.0, 0.0, 1.5, 1, AbrReason::AlreadyOptimal, false)]
    #[case("upswitch_blocked_by_min_buffer", 0, 2_000_000/8, 2.0, 0.0, 10.0, 1.5, 0, AbrReason::BufferTooLowForUpSwitch, false)]
    #[case("upswitch_allowed_with_buffer",   0, 2_000_000/8, 15.0, 0.0, 10.0, 1.5, 2, AbrReason::UpSwitch, true)]
    #[case("safety_factor_tight_blocks_up",  0, 400_000/8, 15.0, 0.0, 0.0, 3.0, 0, AbrReason::AlreadyOptimal, false)]
    #[case("safety_factor_loose_allows_up",  0, 2_000_000/8, 15.0, 0.0, 0.0, 1.0, 2, AbrReason::UpSwitch, true)]
    #[case("down_buf_threshold_triggers",    1, 600_000/8, 4.0, 5.0, 0.0, 1.5, 0, AbrReason::DownSwitch, true)]
    #[case("down_buf_threshold_safe",        1, 800_000/8, 15.0, 5.0, 0.0, 1.5, 1, AbrReason::AlreadyOptimal, false)]
    fn test_throughput_based_switching(
        #[case] _name: &str,
        #[case] start_variant: usize,
        #[case] throughput_bytes: u64,
        #[case] buffer_secs: f64,
        #[case] down_switch_buf: f64,
        #[case] min_buf_up: f64,
        #[case] safety_factor: f64,
        #[case] expected_variant: usize,
        #[case] expected_reason: AbrReason,
        #[case] expected_changed: bool,
    ) {
        let cfg = AbrOptions {
            down_switch_buffer_secs: down_switch_buf,
            min_buffer_for_up_switch_secs: min_buf_up,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(start_variant)),
            throughput_safety_factor: safety_factor,
            variants: variants(),
            ..AbrOptions::default()
        };
        let ctrl = AbrController::new(cfg);
        let now = Instant::now();
        ctrl.push_sample(ThroughputSample {
            bytes: throughput_bytes,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
            content_duration: if buffer_secs > 0.0 {
                Some(Duration::from_secs_f64(buffer_secs))
            } else {
                None
            },
        });
        let d = ctrl.decide(now);
        assert_eq!(d.target_variant_index, expected_variant, "variant mismatch");
        assert_eq!(d.reason, expected_reason, "reason mismatch");
        assert_eq!(d.changed, expected_changed, "changed mismatch");
    }

    #[kithara::test]
    fn upswitch_requires_buffer_and_hysteresis() {
        let ctrl = AbrController::with_estimator(
            AbrOptions {
                mode: AbrMode::Auto(Some(0)),
                variants: variants(),
                min_switch_interval: Duration::from_secs(0),
                min_buffer_for_up_switch_secs: 10.0,
                ..AbrOptions::default()
            },
            estimator_static(Some(1_500_000), 2.0),
        );
        let d = ctrl.decide(Instant::now());
        assert_eq!(d.reason, AbrReason::BufferTooLowForUpSwitch);
        assert!(!d.changed);
    }

    #[kithara::test]
    fn min_switch_interval_prevents_oscillation() {
        let ctrl = AbrController::with_estimator(
            AbrOptions {
                mode: AbrMode::Auto(Some(0)),
                variants: variants(),
                min_switch_interval: Duration::from_secs(30),
                ..AbrOptions::default()
            },
            estimator_static(Some(1_500_000), 15.0),
        );
        let now = Instant::now();
        let d1 = ctrl.decide(now);
        assert!(d1.changed);
        ctrl.apply(&d1, now);
        let d2 = ctrl.decide(now + Duration::from_secs(1));
        assert_eq!(d2.reason, AbrReason::MinInterval);
    }

    #[kithara::test]
    fn no_change_without_estimate() {
        let ctrl = AbrController::with_estimator(
            opts(AbrMode::Auto(Some(1))),
            estimator_static(None, 5.0),
        );
        let d = ctrl.decide(Instant::now());
        assert_eq!(d.reason, AbrReason::NoEstimate);
        assert!(!d.changed);
    }

    #[kithara::test]
    fn test_estimator_called_once_per_decide() {
        let estimator = Unimock::new((
            EstimatorMock::estimate_bps
                .next_call(matching!())
                .returns(Some(500_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(10.0),
        ));
        let ctrl = AbrController::with_estimator(opts(AbrMode::Auto(Some(0))), estimator);
        let _ = ctrl.decide(Instant::now());
    }

    #[kithara::test]
    fn test_min_interval_skips_estimator_call() {
        let ctrl = AbrController::new(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            variants: variants(),
            min_switch_interval: Duration::from_secs(30),
            ..AbrOptions::default()
        });
        ctrl.last_switch_at_nanos
            .store(ctrl.instant_to_nanos(Instant::now()), Ordering::Release);
        assert_eq!(ctrl.decide(Instant::now()).reason, AbrReason::MinInterval);
    }

    #[kithara::test]
    fn apply_no_change_leaves_variant_and_timestamp_unchanged() {
        let ctrl = AbrController::with_estimator(opts(AbrMode::Auto(Some(1))), Unimock::new(()));
        ctrl.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::AlreadyOptimal,
                changed: false,
            },
            Instant::now(),
        );
        assert_eq!(ctrl.get_current_variant_index(), 1);
        assert_eq!(
            ctrl.last_switch_at_nanos.load(Ordering::Acquire),
            AbrController::<Unimock>::NO_SWITCH
        );
    }

    #[kithara::test]
    fn apply_with_change_updates_variant_and_records_switch() {
        let mut cfg = opts(AbrMode::Auto(Some(0)));
        cfg.min_switch_interval = Duration::from_secs(30);
        let ctrl = AbrController::with_estimator(cfg, Unimock::new(()));
        let now = Instant::now();
        ctrl.apply(
            &AbrDecision {
                target_variant_index: 2,
                reason: AbrReason::UpSwitch,
                changed: true,
            },
            now,
        );
        assert_eq!(ctrl.get_current_variant_index(), 2);
        assert_ne!(
            ctrl.last_switch_at_nanos.load(Ordering::Acquire),
            AbrController::<Unimock>::NO_SWITCH
        );
        assert!(!ctrl.can_switch_now(now));
    }

    #[kithara::test]
    fn apply_round_trip_decide_reflects_new_state() {
        let ctrl = AbrController::with_estimator(
            opts(AbrMode::Auto(Some(0))),
            estimator_static(Some(1_500_000), 15.0),
        );
        let d1 = ctrl.decide(Instant::now());
        assert!(d1.changed);
        ctrl.apply(&d1, Instant::now());
        let d2 = ctrl.decide(Instant::now() + Duration::from_secs(60));
        assert_eq!(d2.target_variant_index, d1.target_variant_index);
        assert!(!d2.changed);
    }

    #[kithara::test]
    fn up_switch_hysteresis_boundary() {
        let bw_1 = 512_000_u64;
        let safety = 1.5;
        let hysteresis = 1.3;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let threshold = ((bw_1 as f64) * safety * hysteresis).ceil() as u64;

        let ctrl_below = AbrController::with_estimator(
            opts(AbrMode::Auto(Some(0))),
            estimator_static(Some(threshold - 1), 15.0),
        );
        let d = ctrl_below.decide(Instant::now());
        assert!(!d.changed || d.target_variant_index <= 1);

        let ctrl_above = AbrController::with_estimator(
            opts(AbrMode::Auto(Some(0))),
            estimator_static(Some(threshold + 1), 15.0),
        );
        assert!(ctrl_above.decide(Instant::now()).changed);
    }

    #[kithara::test]
    fn down_switch_hysteresis_boundary() {
        let bw_2 = 1_024_000_u64;
        let safety = 1.5;
        let hysteresis = 0.8;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let threshold = ((bw_2 as f64) * safety * hysteresis).floor() as u64;

        let ctrl_above = AbrController::with_estimator(
            opts(AbrMode::Auto(Some(2))),
            estimator_static(Some(threshold + 1), 15.0),
        );
        assert!(!ctrl_above.decide(Instant::now()).changed);

        let ctrl_below = AbrController::with_estimator(
            opts(AbrMode::Auto(Some(2))),
            estimator_static(Some(threshold - 1), 15.0),
        );
        assert!(ctrl_below.decide(Instant::now()).changed);
    }

    #[kithara::test]
    fn buffer_level_threshold_for_up_switch() {
        let ctrl = AbrController::with_estimator(
            AbrOptions {
                mode: AbrMode::Auto(Some(0)),
                variants: variants(),
                min_switch_interval: Duration::from_secs(0),
                min_buffer_for_up_switch_secs: 10.0,
                ..AbrOptions::default()
            },
            estimator_static(Some(1_500_000), 9.99),
        );
        assert_eq!(
            ctrl.decide(Instant::now()).reason,
            AbrReason::BufferTooLowForUpSwitch
        );
    }

    #[kithara::test]
    fn max_bandwidth_filters_variants() {
        let ctrl = AbrController::with_estimator(
            AbrOptions {
                mode: AbrMode::Auto(Some(0)),
                variants: variants(),
                max_bandwidth_bps: Some(300_000),
                min_switch_interval: Duration::from_secs(0),
                ..AbrOptions::default()
            },
            estimator_static(Some(2_000_000), 15.0),
        );
        assert_eq!(ctrl.decide(Instant::now()).target_variant_index, 0);
    }

    #[kithara::test]
    fn max_bandwidth_forces_downswitch_from_above_cap() {
        let ctrl = AbrController::with_estimator(
            AbrOptions {
                mode: AbrMode::Auto(Some(2)),
                variants: variants(),
                max_bandwidth_bps: Some(600_000),
                min_switch_interval: Duration::from_secs(0),
                ..AbrOptions::default()
            },
            estimator_static(Some(2_000_000), 15.0),
        );
        let d = ctrl.decide(Instant::now());
        assert!(d.changed);
        assert!(d.target_variant_index < 2);
    }

    #[kithara::test]
    fn max_bandwidth_below_all_variants_picks_none() {
        let ctrl = AbrController::with_estimator(
            AbrOptions {
                mode: AbrMode::Auto(Some(0)),
                variants: variants(),
                max_bandwidth_bps: Some(100_000),
                min_switch_interval: Duration::from_secs(0),
                ..AbrOptions::default()
            },
            estimator_static(Some(2_000_000), 15.0),
        );
        assert!(!ctrl.decide(Instant::now()).changed);
    }
}
