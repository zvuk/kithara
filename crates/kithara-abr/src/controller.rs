use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

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
    pub(crate) cfg: AbrOptions,
    estimator: E,
    current_variant: Arc<AtomicUsize>,
    /// Reference instant for computing elapsed time (created at controller init).
    reference_instant: Instant,
    /// Nanoseconds since `reference_instant` of last switch, or NO_SWITCH if none.
    last_switch_at_nanos: AtomicU64,
}

impl<E: Estimator> AbrController<E> {
    pub fn with_estimator(cfg: AbrOptions, estimator: E) -> Self {
        let initial_variant = cfg.initial_variant();
        Self {
            cfg,
            estimator,
            current_variant: Arc::new(AtomicUsize::new(initial_variant)),
            reference_instant: Instant::now(),
            last_switch_at_nanos: AtomicU64::new(NO_SWITCH),
        }
    }

    /// Convert Instant to nanos since reference. Returns at least 1 to distinguish from NO_SWITCH.
    fn instant_to_nanos(&self, instant: Instant) -> u64 {
        let nanos = instant
            .saturating_duration_since(self.reference_instant)
            .as_nanos() as u64;
        // Ensure we never return 0 (which means "no switch")
        nanos.max(1)
    }

    /// Convert nanos to Instant. Returns None if value is NO_SWITCH.
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

    pub fn current_variant(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.current_variant)
    }

    /// Get current variant index.
    ///
    /// This is a convenience method to get the current variant index
    /// without dealing with atomics.
    pub fn get_current_variant_index(&self) -> usize {
        self.current_variant.load(Ordering::Acquire)
    }

    pub fn set_current_variant(&mut self, variant_index: usize) {
        self.current_variant.store(variant_index, Ordering::Release);
    }

    /// Switch to manual mode with specified variant.
    ///
    /// This disables ABR and locks playback to the specified variant index.
    /// The variant switch takes effect on the next `decide()` call.
    pub fn set_manual_variant(&mut self, variant_index: usize) {
        self.cfg.mode = AbrMode::Manual(variant_index);
    }

    /// Switch to automatic mode.
    ///
    /// This enables ABR algorithm. Optionally specify initial variant index
    /// (defaults to current variant if None).
    pub fn set_auto_mode(&mut self, initial_variant: Option<usize>) {
        let initial = initial_variant.unwrap_or_else(|| self.get_current_variant_index());
        self.cfg.mode = AbrMode::Auto(Some(initial));
    }

    /// Get current ABR mode.
    pub fn get_mode(&self) -> AbrMode {
        self.cfg.mode
    }

    /// Check if ABR is enabled (Auto mode).
    pub fn is_auto(&self) -> bool {
        self.cfg.is_auto()
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
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::MinInterval,
                changed: false,
            };
        }

        let Some(estimate_bps) = self.estimator.estimate_bps() else {
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
            .map(|v| v.bandwidth_bps)
            .unwrap_or(0);

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

        // Adjust throughput by safety factor
        let adjusted_bps = (estimate_bps as f64 * self.cfg.throughput_safety_factor).max(0.0);

        // Best candidate not exceeding adjusted throughput, otherwise lowest
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

        // Up-switch path
        if candidate_bw > current_bw {
            let buffer_ok = self.cfg.min_buffer_for_up_switch_secs <= 0.0
                || buffer_level_secs >= self.cfg.min_buffer_for_up_switch_secs;
            let headroom_ok = adjusted_bps >= (candidate_bw as f64) * self.cfg.up_hysteresis_ratio;
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
            .map(|t| now.duration_since(t) >= self.cfg.min_switch_interval)
            .unwrap_or(true)
    }
}

// Backward compatibility: Default AbrController with ThroughputEstimator
impl AbrController<ThroughputEstimator> {
    pub fn new(cfg: AbrOptions) -> Self {
        let estimator = ThroughputEstimator::new(&cfg);
        Self::with_estimator(cfg, estimator)
    }
}

/// Type alias for backward compatibility.
/// Use `AbrController<ThroughputEstimator>` or this alias for production code.
/// Use `AbrController<MockEstimator>` in tests for isolated testing.
pub type DefaultAbrController = AbrController<ThroughputEstimator>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;

    use super::{
        super::{estimator::MockEstimator, types::Variant},
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
            throughput_safety_factor: 1.5,
            min_buffer_for_up_switch_secs: 0.0,
            down_switch_buffer_secs: 0.0,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(initial_variant)),
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
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            min_switch_interval: Duration::ZERO,
            mode: AbrMode::Auto(Some(0)),
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
            min_switch_interval: Duration::from_secs(30),
            min_buffer_for_up_switch_secs: 0.0,
            down_switch_buffer_secs: 0.0,
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
            mode: AbrMode::Auto(Some(1)),
            min_switch_interval: Duration::ZERO,
            variants: variants(),
            ..AbrOptions::default()
        };

        let mut mock_estimator = MockEstimator::new();

        // Setup expectations: estimate_bps() will be called exactly 2 times
        mock_estimator
            .expect_estimate_bps()
            .times(2) // Built-in call count verification!
            .returning(|| Some(1_000_000));

        mock_estimator.expect_buffer_level_secs().returning(|| 0.0);

        // push_sample() won't be called in this test
        mock_estimator.expect_push_sample().times(0);

        let c = AbrController::with_estimator(cfg, mock_estimator);
        let now = Instant::now();

        // First decide - calls estimator once
        c.decide(now);

        // Second decide - calls estimator again
        c.decide(now);

        // Mockall automatically verifies call counts on drop - no manual assert needed!
    }

    #[test]
    fn test_min_interval_skips_estimator_call() {
        let cfg = AbrOptions {
            mode: AbrMode::Auto(Some(1)),
            min_switch_interval: Duration::from_secs(30),
            variants: variants(),
            ..AbrOptions::default()
        };

        let mut mock_estimator = MockEstimator::new();

        // Setup expectations: estimate_bps() will be called exactly ONCE
        // (second decide() should early-return due to min_interval)
        mock_estimator
            .expect_estimate_bps()
            .times(1) // Only first call triggers estimator
            .returning(|| Some(5_000_000));

        mock_estimator.expect_buffer_level_secs().returning(|| 20.0);

        mock_estimator.expect_push_sample().times(0);

        let c = AbrController::with_estimator(cfg, mock_estimator);
        let now = Instant::now();

        // First call - should call estimator and cause switch
        let d1 = c.decide(now);
        assert!(d1.changed, "First call should switch");

        // Second call immediately - should NOT call estimator (min_interval not elapsed)
        let d2 = c.decide(now);
        assert!(!d2.changed, "Second call should not switch");
        assert_eq!(d2.reason, AbrReason::MinInterval);

        // Mockall automatically verifies estimator was called exactly once
    }

    #[test]
    fn test_abr_sequence_estimate_then_push() {
        use mockall::Sequence;

        let cfg = AbrOptions {
            mode: AbrMode::Auto(Some(1)),
            min_switch_interval: Duration::ZERO,
            variants: variants(),
            ..AbrOptions::default()
        };

        let mut seq = Sequence::new();
        let mut mock_estimator = MockEstimator::new();

        // Verify sequence: estimate → push_sample → estimate
        mock_estimator.expect_buffer_level_secs().returning(|| 0.0);

        mock_estimator
            .expect_estimate_bps()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Some(1_000_000));

        mock_estimator
            .expect_push_sample()
            .times(1)
            .in_sequence(&mut seq)
            .return_const(());

        mock_estimator
            .expect_estimate_bps()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Some(2_000_000));

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
        use mockall::Sequence;

        let cfg = AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            min_switch_interval: Duration::ZERO,
            variants: variants(),
            ..AbrOptions::default()
        };

        let mut seq = Sequence::new();
        let mut mock_estimator = MockEstimator::new();

        mock_estimator.expect_buffer_level_secs().returning(|| 0.0);

        mock_estimator
            .expect_estimate_bps()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Some(500_000));

        mock_estimator
            .expect_estimate_bps()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Some(1_500_000));

        mock_estimator
            .expect_estimate_bps()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Some(3_000_000));

        let c = AbrController::with_estimator(cfg, mock_estimator);
        let now = Instant::now();

        c.decide(now);
        c.decide(now);
        c.decide(now);
    }
}
