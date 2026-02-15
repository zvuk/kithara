use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use web_time::Instant;

use super::{AbrMode, AbrOptions, Estimator, ThroughputEstimator, ThroughputSample};

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbrDecision {
    pub target_variant_index: usize,
    pub reason: AbrReason,
    pub changed: bool,
}

/// Value indicating no switch has occurred yet.
const NO_SWITCH: u64 = 0;

pub struct AbrController<E: Estimator> {
    cfg: AbrOptions,
    current_variant: Arc<AtomicUsize>,
    estimator: E,
    /// Nanoseconds since `reference_instant` of last switch, or `NO_SWITCH` if none.
    last_switch_at_nanos: AtomicU64,
    /// Reference instant for computing elapsed time (created at controller init).
    reference_instant: Instant,
}

impl<E: Estimator> AbrController<E> {
    pub fn with_estimator(cfg: AbrOptions, estimator: E) -> Self {
        let initial_variant = cfg.initial_variant();
        Self {
            cfg,
            current_variant: Arc::new(AtomicUsize::new(initial_variant)),
            estimator,
            last_switch_at_nanos: AtomicU64::new(NO_SWITCH),
            reference_instant: Instant::now(),
        }
    }

    /// Convert Instant to nanos since reference. Returns at least 1 to distinguish from `NO_SWITCH`.
    #[expect(clippy::cast_possible_truncation)]
    // elapsed nanos fit u64 for any practical time span (585 years)
    fn instant_to_nanos(&self, instant: Instant) -> u64 {
        let nanos = instant
            .saturating_duration_since(self.reference_instant)
            .as_nanos() as u64;
        // Ensure we never return 0 (which means "no switch")
        nanos.max(1)
    }

    /// Convert nanos to Instant. Returns None if value is `NO_SWITCH`.
    fn nanos_to_instant(&self, nanos: u64) -> Option<Instant> {
        if nanos == NO_SWITCH {
            None
        } else {
            Some(self.reference_instant + Duration::from_nanos(nanos))
        }
    }

    /// Record a switch at the given instant.
    fn record_switch(&self, now: Instant) {
        self.last_switch_at_nanos
            .store(self.instant_to_nanos(now), Ordering::Release);
    }

    /// Get current variant index.
    pub fn get_current_variant_index(&self) -> usize {
        self.current_variant.load(Ordering::Acquire)
    }

    pub fn push_throughput_sample(&mut self, sample: ThroughputSample) {
        self.estimator.push_sample(sample);
    }

    /// Reset buffer level (e.g., on seek).
    pub fn reset_buffer(&mut self) {
        self.estimator.reset_buffer();
    }

    /// Get current buffer level in seconds.
    pub fn buffer_level_secs(&self) -> f64 {
        self.estimator.buffer_level_secs()
    }

    /// Make ABR decision using variants from configuration.
    #[expect(
        clippy::cognitive_complexity,
        reason = "ABR decision logic with multiple branches"
    )]
    pub fn decide(&self, now: Instant) -> AbrDecision {
        let current = self.current_variant.load(Ordering::Acquire);

        // Handle Manual mode - always return configured variant
        if let AbrMode::Manual(idx) = self.cfg.mode {
            return AbrDecision {
                target_variant_index: idx,
                reason: AbrReason::ManualOverride,
                changed: idx != current,
            };
        }

        let buffer_level_secs = self.buffer_level_secs();

        if !self.can_switch_now(now) {
            tracing::debug!(
                current,
                buffer_level_secs,
                "ABR decide: MinInterval not elapsed"
            );
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::MinInterval,
                changed: false,
            };
        }

        let Some(estimate_bps) = self.estimator.estimate_bps() else {
            tracing::debug!(current, buffer_level_secs, "ABR decide: NoEstimate");
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::NoEstimate,
                changed: false,
            };
        };

        // Get current variant bandwidth from config
        let current_bw = self
            .cfg
            .variants
            .iter()
            .find(|v| v.variant_index == current)
            .map_or(0, |v| v.bandwidth_bps);

        // Collect all variants as (index, bandwidth) pairs and sort by bandwidth
        let mut variants: Vec<(usize, u64)> = self
            .cfg
            .variants
            .iter()
            .map(|v| (v.variant_index, v.bandwidth_bps))
            .collect();

        if variants.is_empty() {
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::AlreadyOptimal,
                changed: false,
            };
        }

        variants.sort_by_key(|(_, bw)| *bw);

        // Adjust throughput by safety factor (divide, not multiply!)
        #[expect(clippy::cast_precision_loss)] // bitrate precision loss is negligible for ABR
        let adjusted_bps = (estimate_bps as f64 / self.cfg.throughput_safety_factor).max(0.0);

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

        // Best candidate not exceeding adjusted throughput, otherwise lowest
        #[expect(clippy::cast_precision_loss)] // bitrate precision loss is negligible for ABR
        let best_under = variants
            .iter()
            .filter(|(_, bw)| (*bw as f64) <= adjusted_bps)
            .max_by_key(|(_, bw)| *bw);
        let candidate = best_under.or_else(|| variants.first());

        let Some(&(candidate_idx, candidate_bw)) = candidate else {
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::AlreadyOptimal,
                changed: false,
            };
        };

        tracing::debug!(
            candidate_idx,
            candidate_bw,
            current,
            current_bw,
            "ABR decide: candidate selected"
        );

        // Up-switch path
        if candidate_bw > current_bw {
            let buffer_ok = self.cfg.min_buffer_for_up_switch_secs <= 0.0
                || buffer_level_secs >= self.cfg.min_buffer_for_up_switch_secs;
            #[expect(clippy::cast_precision_loss)] // bitrate precision loss is negligible for ABR
            let headroom_ok = adjusted_bps >= (candidate_bw as f64) * self.cfg.up_hysteresis_ratio;
            #[expect(clippy::cast_precision_loss)] // bitrate precision loss is negligible for ABR
            let required_bps = (candidate_bw as f64) * self.cfg.up_hysteresis_ratio;
            tracing::debug!(
                buffer_ok,
                headroom_ok,
                buffer_level_secs,
                min_buffer = self.cfg.min_buffer_for_up_switch_secs,
                adjusted_bps,
                required_bps,
                "ABR decide: up-switch check"
            );
            if buffer_ok && headroom_ok {
                self.record_switch(now);
                return AbrDecision {
                    target_variant_index: candidate_idx,
                    reason: AbrReason::UpSwitch,
                    changed: true,
                };
            }
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::BufferTooLowForUpSwitch,
                changed: false,
            };
        }

        // Down-switch path
        if candidate_bw < current_bw {
            let urgent_down = buffer_level_secs <= self.cfg.down_switch_buffer_secs;
            #[expect(clippy::cast_precision_loss)] // bitrate precision loss is negligible for ABR
            let margin_ok = adjusted_bps <= (current_bw as f64) * self.cfg.down_hysteresis_ratio;
            if urgent_down || margin_ok {
                self.record_switch(now);
                return AbrDecision {
                    target_variant_index: candidate_idx,
                    reason: AbrReason::DownSwitch,
                    changed: true,
                };
            }
        }

        AbrDecision {
            target_variant_index: current,
            reason: AbrReason::AlreadyOptimal,
            changed: false,
        }
    }

    pub fn apply(&mut self, decision: &AbrDecision, now: Instant) {
        let current = self.current_variant.load(Ordering::Acquire);
        if decision.target_variant_index == current {
            return;
        }
        self.current_variant
            .store(decision.target_variant_index, Ordering::Release);
        self.record_switch(now);
    }

    fn can_switch_now(&self, now: Instant) -> bool {
        let nanos = self.last_switch_at_nanos.load(Ordering::Acquire);
        self.nanos_to_instant(nanos)
            .is_none_or(|t| now.duration_since(t) >= self.cfg.min_switch_interval)
    }
}

// Backward compatibility: Default AbrController with ThroughputEstimator
impl AbrController<ThroughputEstimator> {
    #[must_use]
    pub fn new(cfg: AbrOptions) -> Self {
        let estimator = ThroughputEstimator::new(&cfg);
        Self::with_estimator(cfg, estimator)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;
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

    #[rstest]
    #[case("downswitch_low_throughput", 2, 300_000 / 8, 10.0, 0, AbrReason::DownSwitch, true)]
    #[case("upswitch_high_throughput", 0, 2_000_000 / 8, 0.0, 2, AbrReason::UpSwitch, true)]
    #[case(
        "downswitch_buffer_too_low",
        2,
        30_000,
        0.1,
        0,
        AbrReason::DownSwitch,
        true
    )]
    fn test_throughput_based_switching(
        #[case] _name: &str,
        #[case] initial_variant: usize,
        #[case] throughput_bytes: u64,
        #[case] buffer_level_secs: f64,
        #[case] expected_variant: usize,
        #[case] expected_reason: AbrReason,
        #[case] expected_changed: bool,
    ) {
        let cfg = AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(initial_variant)),
            throughput_safety_factor: 1.5,
            variants: variants(),
            ..AbrOptions::default()
        };

        let mut c = AbrController::new(cfg);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: throughput_bytes,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
            content_duration: if buffer_level_secs > 0.0 {
                Some(Duration::from_secs_f64(buffer_level_secs))
            } else {
                None
            },
        });

        let d = c.decide(now);
        assert_eq!(d.target_variant_index, expected_variant);
        assert_eq!(d.reason, expected_reason);
        assert_eq!(d.changed, expected_changed);
    }

    #[test]
    fn upswitch_requires_buffer_and_hysteresis() {
        let cfg = AbrOptions {
            min_buffer_for_up_switch_secs: 10.0,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            variants: variants(),
            ..AbrOptions::default()
        };

        let mut c = AbrController::new(cfg);
        let now = Instant::now();

        // Low buffer: should not up-switch
        c.push_throughput_sample(ThroughputSample {
            bytes: 2_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
            content_duration: Some(Duration::from_secs_f64(2.0)),
        });
        let low_buf = c.decide(now);
        assert_eq!(low_buf.target_variant_index, 0);
        assert_eq!(low_buf.reason, AbrReason::BufferTooLowForUpSwitch);

        // High buffer: should up-switch
        c.reset_buffer();
        c.push_throughput_sample(ThroughputSample {
            bytes: 2_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
            content_duration: Some(Duration::from_secs_f64(20.0)),
        });
        let ok_buf = c.decide(now);
        assert_eq!(ok_buf.target_variant_index, 2);
        assert_eq!(ok_buf.reason, AbrReason::UpSwitch);
        assert!(ok_buf.changed);
    }

    #[test]
    fn min_switch_interval_prevents_oscillation() {
        let cfg = AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_secs(30),
            mode: AbrMode::Auto(Some(1)),
            variants: variants(),
            ..AbrOptions::default()
        };

        let mut c = AbrController::new(cfg);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: 2_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
            content_duration: Some(Duration::from_secs_f64(10.0)),
        });

        let d1 = c.decide(now);
        assert_eq!(d1.target_variant_index, 2);
        assert!(d1.changed);

        let d2 = c.decide(now);
        assert_eq!(d2.target_variant_index, 1);
        assert!(!d2.changed);
        assert_eq!(d2.reason, AbrReason::MinInterval);
    }

    #[test]
    fn no_change_without_estimate() {
        let cfg = AbrOptions {
            mode: AbrMode::Auto(Some(1)),
            variants: variants(),
            ..AbrOptions::default()
        };
        let c = AbrController::new(cfg);
        let now = Instant::now();

        let d = c.decide(now);
        assert_eq!(d.target_variant_index, 1);
        assert!(!d.changed);
        assert_eq!(d.reason, AbrReason::NoEstimate);
    }

    #[test]
    fn test_estimator_called_once_per_decide() {
        let cfg = AbrOptions {
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(1)),
            variants: variants(),
            ..AbrOptions::default()
        };

        // estimate_bps() called exactly 2 times, buffer_level_secs() called freely
        // push_sample() is never set up — unimock panics if called unexpectedly
        let mock_estimator = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(1_000_000))
                .n_times(2),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
        ));

        let c = AbrController::with_estimator(cfg, mock_estimator);
        let now = Instant::now();

        // First decide — calls estimator once
        c.decide(now);

        // Second decide — calls estimator again
        c.decide(now);

        // Unimock verifies call counts on drop
    }

    #[test]
    fn test_min_interval_skips_estimator_call() {
        let cfg = AbrOptions {
            min_switch_interval: Duration::from_secs(30),
            mode: AbrMode::Auto(Some(1)),
            variants: variants(),
            ..AbrOptions::default()
        };

        // estimate_bps() called exactly ONCE
        // (second decide() early-returns due to min_interval)
        let mock_estimator = Unimock::new((
            EstimatorMock::estimate_bps
                .some_call(matching!())
                .returns(Some(5_000_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(20.0),
        ));

        let c = AbrController::with_estimator(cfg, mock_estimator);
        let now = Instant::now();

        // First call — calls estimator and causes switch
        let d1 = c.decide(now);
        assert!(d1.changed, "First call should switch");

        // Second call immediately — should NOT call estimator (min_interval)
        let d2 = c.decide(now);
        assert!(!d2.changed, "Second call should not switch");
        assert_eq!(d2.reason, AbrReason::MinInterval);

        // Unimock verifies estimator was called exactly once on drop
    }

    #[test]
    fn test_abr_sequence_estimate_then_push() {
        let cfg = AbrOptions {
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(1)),
            variants: variants(),
            ..AbrOptions::default()
        };

        // Verify sequence: estimate → push_sample → estimate
        // buffer_level_secs is a stub (called any time, not sequenced)
        let mock_estimator = Unimock::new((
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
            EstimatorMock::estimate_bps
                .next_call(matching!())
                .returns(Some(1_000_000)),
            EstimatorMock::push_sample
                .next_call(matching!(_))
                .returns(()),
            EstimatorMock::estimate_bps
                .next_call(matching!())
                .returns(Some(2_000_000)),
        ));

        let mut c = AbrController::with_estimator(cfg, mock_estimator);
        let now = Instant::now();

        c.decide(now);

        c.push_throughput_sample(ThroughputSample {
            bytes: 1_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
            content_duration: None,
        });

        c.decide(now);
    }

    #[test]
    fn test_abr_sequence_multiple_decisions() {
        let cfg = AbrOptions {
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
            variants: variants(),
            ..AbrOptions::default()
        };

        // Three ordered estimate_bps calls with different return values
        let mock_estimator = Unimock::new((
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
            EstimatorMock::estimate_bps
                .next_call(matching!())
                .returns(Some(500_000)),
            EstimatorMock::estimate_bps
                .next_call(matching!())
                .returns(Some(1_500_000)),
            EstimatorMock::estimate_bps
                .next_call(matching!())
                .returns(Some(3_000_000)),
        ));

        let c = AbrController::with_estimator(cfg, mock_estimator);
        let now = Instant::now();

        c.decide(now);
        c.decide(now);
        c.decide(now);
    }

    // apply() tests

    #[test]
    fn apply_no_change_leaves_variant_and_timestamp_unchanged() {
        let cfg = AbrOptions {
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(1)),
            variants: variants(),
            ..AbrOptions::default()
        };

        // adjusted_bps = 768_000 / 1.5 = 512_000 → variant 1 (512k) is best-under
        // candidate == current → AlreadyOptimal, no switch
        let mock = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(768_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
        ));

        let mut c = AbrController::with_estimator(cfg, mock);
        let now = Instant::now();

        let d = c.decide(now);
        assert!(!d.changed);
        assert_eq!(d.target_variant_index, 1);

        c.apply(&d, now);

        // Variant stays at 1
        assert_eq!(c.get_current_variant_index(), 1);
        // No switch recorded → can_switch_now should remain true
        assert!(c.can_switch_now(now));
    }

    #[test]
    fn apply_with_change_updates_variant_and_records_switch() {
        let cfg = AbrOptions {
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_secs(30),
            mode: AbrMode::Auto(Some(0)),
            variants: variants(),
            ..AbrOptions::default()
        };

        // High throughput to trigger up-switch from variant 0
        let mock = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(5_000_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
        ));

        let mut c = AbrController::with_estimator(cfg, mock);
        let now = Instant::now();

        let d = c.decide(now);
        assert!(d.changed);

        c.apply(&d, now);

        // Variant updated
        assert_eq!(c.get_current_variant_index(), d.target_variant_index);
        // Switch timestamp recorded → can_switch_now returns false immediately
        assert!(!c.can_switch_now(now));
    }

    #[test]
    fn apply_round_trip_decide_reflects_new_state() {
        let cfg = AbrOptions {
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
            variants: variants(),
            ..AbrOptions::default()
        };

        // First call: high throughput → up-switch to variant 2
        // Second call: same throughput → variant 2 is now current, AlreadyOptimal
        let mock = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(5_000_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
        ));

        let mut c = AbrController::with_estimator(cfg, mock);
        let now = Instant::now();

        let d1 = c.decide(now);
        assert!(d1.changed);
        assert_eq!(d1.target_variant_index, 2);

        c.apply(&d1, now);

        let d2 = c.decide(now);
        // Already on best variant — no change
        assert!(!d2.changed);
        assert_eq!(d2.target_variant_index, 2);
        assert_eq!(d2.reason, AbrReason::AlreadyOptimal);
    }

    // Hysteresis boundary tests

    #[test]
    fn up_switch_hysteresis_boundary() {
        // variants: 0 → 256k, 1 → 512k, 2 → 1024k
        // Current: variant 0 (256k bps)
        // Candidate for up-switch: variant 2 (1024k bps)
        //
        // Up-switch condition: adjusted_bps >= candidate_bw * up_hysteresis_ratio
        // adjusted_bps = estimate_bps / safety_factor
        //
        // Threshold estimate = candidate_bw * up_hysteresis * safety_factor
        //                    = 1_024_000 * 1.3 * 1.5 = 1_996_800 bps
        let safety = 1.5;
        let up_hysteresis = 1.3;
        let candidate_bw: u64 = 1_024_000;
        let threshold_bps: u64 = (candidate_bw as f64 * up_hysteresis * safety) as u64;

        let base_cfg = AbrOptions {
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: safety,
            up_hysteresis_ratio: up_hysteresis,
            variants: variants(),
            ..AbrOptions::default()
        };

        // At exact threshold → adjusted_bps == candidate_bw * up_hysteresis → switch
        // But due to integer truncation, threshold_bps / safety_factor may lose precision.
        // Use threshold - 1 to guarantee NO switch.
        let mock_below = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(threshold_bps - 1)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
        ));
        let c = AbrController::with_estimator(base_cfg.clone(), mock_below);
        let d = c.decide(Instant::now());
        assert_ne!(
            d.reason,
            AbrReason::UpSwitch,
            "Should NOT up-switch at threshold - 1 bps"
        );

        // threshold + 1 → guaranteed switch
        let mock_above = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(threshold_bps + 1)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
        ));
        let c = AbrController::with_estimator(base_cfg, mock_above);
        let d = c.decide(Instant::now());
        assert_eq!(
            d.reason,
            AbrReason::UpSwitch,
            "Should up-switch at threshold + 1 bps"
        );
        assert!(d.changed);
        assert_eq!(d.target_variant_index, 2);
    }

    #[test]
    fn down_switch_hysteresis_boundary() {
        // Current: variant 2 (1_024_000 bps)
        // Down-switch margin condition: adjusted_bps <= current_bw * down_hysteresis_ratio
        // adjusted_bps = estimate_bps / safety_factor
        //
        // Threshold estimate = current_bw * down_hysteresis * safety_factor
        //                    = 1_024_000 * 0.8 * 1.5 = 1_228_800 bps
        let safety = 1.5;
        let down_hysteresis = 0.8;
        let current_bw: u64 = 1_024_000;
        let threshold_bps: u64 = (current_bw as f64 * down_hysteresis * safety) as u64;

        let base_cfg = AbrOptions {
            down_hysteresis_ratio: down_hysteresis,
            down_switch_buffer_secs: 0.0, // disable urgent-down path
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(2)),
            throughput_safety_factor: safety,
            variants: variants(),
            ..AbrOptions::default()
        };

        // At threshold → adjusted = threshold / safety = current_bw * down_hysteresis → switch
        let mock_at = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(threshold_bps)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(10.0), // high buffer, no urgent-down
        ));
        let c = AbrController::with_estimator(base_cfg.clone(), mock_at);
        let d = c.decide(Instant::now());
        assert_eq!(
            d.reason,
            AbrReason::DownSwitch,
            "Should down-switch at threshold bps"
        );
        assert!(d.changed);

        // threshold + 1 → adjusted just above margin → no switch
        let mock_above = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(threshold_bps + 1)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(10.0),
        ));
        let c = AbrController::with_estimator(base_cfg, mock_above);
        let d = c.decide(Instant::now());
        assert_ne!(
            d.reason,
            AbrReason::DownSwitch,
            "Should NOT down-switch at threshold + 1 bps"
        );
    }

    // Buffer threshold for up-switch

    #[test]
    fn buffer_level_threshold_for_up_switch() {
        let min_buffer = 10.0;

        let base_cfg = AbrOptions {
            min_buffer_for_up_switch_secs: min_buffer,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            variants: variants(),
            ..AbrOptions::default()
        };

        // Buffer exactly at threshold → allowed
        let mock_ok = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(5_000_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(min_buffer),
        ));
        let c = AbrController::with_estimator(base_cfg.clone(), mock_ok);
        let d = c.decide(Instant::now());
        assert_eq!(
            d.reason,
            AbrReason::UpSwitch,
            "Should allow at exact threshold"
        );
        assert!(d.changed);

        // Buffer slightly below threshold → rejected
        let mock_low = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(5_000_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(min_buffer - 0.001),
        ));
        let c = AbrController::with_estimator(base_cfg, mock_low);
        let d = c.decide(Instant::now());
        assert_eq!(
            d.reason,
            AbrReason::BufferTooLowForUpSwitch,
            "Should reject when buffer slightly below threshold"
        );
        assert!(!d.changed);
    }

    // Single variant

    #[test]
    fn single_variant_returns_already_optimal() {
        let cfg = AbrOptions {
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
            variants: vec![Variant {
                variant_index: 0,
                bandwidth_bps: 256_000,
            }],
            ..AbrOptions::default()
        };

        let mock = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(5_000_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(20.0),
        ));

        let c = AbrController::with_estimator(cfg, mock);
        let d = c.decide(Instant::now());
        assert_eq!(d.target_variant_index, 0);
        assert_eq!(d.reason, AbrReason::AlreadyOptimal);
        assert!(!d.changed);
    }

    // Min-interval enforcement

    #[test]
    fn min_interval_enforcement_precise() {
        let interval = Duration::from_secs(30);
        let cfg = AbrOptions {
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: interval,
            mode: AbrMode::Auto(Some(0)),
            variants: variants(),
            ..AbrOptions::default()
        };

        let mock = Unimock::new((
            EstimatorMock::estimate_bps
                .each_call(matching!())
                .returns(Some(5_000_000)),
            EstimatorMock::buffer_level_secs
                .each_call(matching!())
                .returns(0.0),
        ));

        let mut c = AbrController::with_estimator(cfg, mock);
        // Use an instant well after reference_instant to avoid nanos edge cases
        let t0 = Instant::now() + Duration::from_secs(1);

        // First decide → triggers up-switch (decide records switch timestamp)
        let d1 = c.decide(t0);
        assert!(d1.changed);
        c.apply(&d1, t0);

        // At interval - 1ms → too soon
        let t1 = t0 + interval - Duration::from_millis(1);
        let d2 = c.decide(t1);
        assert_eq!(
            d2.reason,
            AbrReason::MinInterval,
            "Should block before interval elapses"
        );
        assert!(!d2.changed);

        // At interval + 1ms → guaranteed allowed (avoids nanos rounding)
        let t2 = t0 + interval + Duration::from_millis(1);
        let d3 = c.decide(t2);
        assert_ne!(
            d3.reason,
            AbrReason::MinInterval,
            "Should allow after interval elapses"
        );
    }
}
