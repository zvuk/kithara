use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use super::{AbrConfig, ThroughputEstimator, ThroughputSample, Variant};
use crate::{options::VariantSelector, playlist::MasterPlaylist};

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

pub struct AbrController {
    cfg: AbrConfig,
    variant_selector: Option<VariantSelector>,
    estimator: ThroughputEstimator,
    current_variant: Arc<AtomicUsize>,
    /// Reference instant for computing elapsed time (created at controller init).
    reference_instant: Instant,
    /// Nanoseconds since `reference_instant` of last switch, or NO_SWITCH if none.
    last_switch_at_nanos: AtomicU64,
}

impl AbrController {
    pub fn new(cfg: AbrConfig, variant_selector: Option<VariantSelector>) -> Self {
        let estimator = ThroughputEstimator::new(&cfg);
        let initial_variant = cfg.initial_variant();
        Self {
            cfg,
            variant_selector,
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

    pub fn set_current_variant(&mut self, variant_index: usize) {
        self.current_variant.store(variant_index, Ordering::Release);
    }

    /// Check if ABR is enabled (Auto mode).
    pub fn is_auto(&self) -> bool {
        self.cfg.is_auto()
    }

    pub fn push_throughput_sample(&mut self, sample: ThroughputSample) {
        self.estimator.push_sample(sample);
    }

    pub fn decide(
        &self,
        variants: &[Variant],
        buffer_level_secs: f64,
        now: Instant,
    ) -> AbrDecision {
        let current = self.current_variant.load(Ordering::Acquire);

        let Some(estimate_bps) = self.estimator.estimate_bps() else {
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::NoEstimate,
                changed: false,
            };
        };

        if !self.can_switch_now(now) {
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::MinInterval,
                changed: false,
            };
        }

        let current_bw = variants
            .iter()
            .find(|v| v.variant_index == current)
            .map(|v| v.bandwidth_bps)
            .unwrap_or(0);

        let mut by_bw: Vec<Variant> = variants.to_vec();
        by_bw.sort_by_key(|v| v.bandwidth_bps);

        // Adjust throughput by safety factor (similar to reference: adjusted = est * safety)
        let adjusted_bps = (estimate_bps as f64 * self.cfg.throughput_safety_factor).max(0.0);

        // Best candidate not exceeding adjusted throughput, otherwise lowest
        let best_under = by_bw
            .iter()
            .filter(|v| (v.bandwidth_bps as f64) <= adjusted_bps)
            .max_by_key(|v| v.bandwidth_bps);
        let fallback = by_bw.first();
        let candidate = best_under.or(fallback);

        // If nothing to consider, stay put.
        let Some(candidate) = candidate else {
            return AbrDecision {
                target_variant_index: current,
                reason: AbrReason::AlreadyOptimal,
                changed: false,
            };
        };

        // Up-switch path
        if candidate.bandwidth_bps > current_bw {
            let buffer_ok = self.cfg.min_buffer_for_up_switch_secs <= 0.0
                || buffer_level_secs >= self.cfg.min_buffer_for_up_switch_secs;
            let headroom_ok =
                adjusted_bps >= (candidate.bandwidth_bps as f64) * self.cfg.up_hysteresis_ratio;
            if buffer_ok && headroom_ok {
                self.record_switch(now);
                return AbrDecision {
                    target_variant_index: candidate.variant_index,
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
        if candidate.bandwidth_bps < current_bw {
            let urgent_down = buffer_level_secs <= self.cfg.down_switch_buffer_secs;
            let margin_ok = adjusted_bps <= (current_bw as f64) * self.cfg.down_hysteresis_ratio;
            if urgent_down || margin_ok {
                self.record_switch(now);
                return AbrDecision {
                    target_variant_index: candidate.variant_index,
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

    pub fn decide_for_master(
        &self,
        master_playlist: &MasterPlaylist,
        variants: &[Variant],
        buffer_level_secs: f64,
        now: Instant,
    ) -> AbrDecision {
        if let Some(selector) = self.variant_selector.as_ref()
            && let Some(manual) = selector(master_playlist)
        {
            return AbrDecision {
                target_variant_index: manual,
                reason: AbrReason::ManualOverride,
                changed: manual != self.current_variant.load(Ordering::Acquire),
            };
        }

        self.decide(variants, buffer_level_secs, now)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

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

    #[test]
    fn downswitch_on_low_throughput() {
        let cfg = AbrConfig {
            throughput_safety_factor: 1.5,
            min_buffer_for_up_switch_secs: 0.0,
            down_switch_buffer_secs: 0.0,
            min_switch_interval: Duration::ZERO,
            mode: crate::options::AbrMode::Auto(Some(2)),
            ..AbrConfig::default()
        };

        let mut c = AbrController::new(cfg, None);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: 300_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
        });

        let d = c.decide(&variants(), 10.0, now);
        assert_eq!(d.target_variant_index, 0);
        assert_eq!(d.reason, AbrReason::DownSwitch);
        assert!(d.changed);
    }

    #[test]
    fn upswitch_requires_buffer_and_hysteresis() {
        let cfg = AbrConfig {
            min_buffer_for_up_switch_secs: 10.0,
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            min_switch_interval: Duration::ZERO,
            mode: crate::options::AbrMode::Auto(Some(0)),
            ..AbrConfig::default()
        };

        let mut c = AbrController::new(cfg, None);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: 2_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
        });

        let low_buf = c.decide(&variants(), 2.0, now);
        assert_eq!(low_buf.target_variant_index, 0);
        assert_eq!(low_buf.reason, AbrReason::BufferTooLowForUpSwitch);

        let ok_buf = c.decide(&variants(), 20.0, now);
        assert_eq!(ok_buf.target_variant_index, 2);
        assert_eq!(ok_buf.reason, AbrReason::UpSwitch);
        assert!(ok_buf.changed);
    }

    #[test]
    fn min_switch_interval_prevents_oscillation() {
        let cfg = AbrConfig {
            min_switch_interval: Duration::from_secs(30),
            min_buffer_for_up_switch_secs: 0.0,
            down_switch_buffer_secs: 0.0,
            mode: crate::options::AbrMode::Auto(Some(1)),
            ..AbrConfig::default()
        };

        let mut c = AbrController::new(cfg, None);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: 2_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
        });

        let d1 = c.decide(&variants(), 10.0, now);
        assert_eq!(d1.target_variant_index, 2);
        assert!(d1.changed);

        let d2 = c.decide(&variants(), 10.0, now);
        assert_eq!(d2.target_variant_index, 1);
        assert!(!d2.changed);
        assert_eq!(d2.reason, AbrReason::MinInterval);
    }

    #[test]
    fn aggressive_up_switch_without_interval() {
        let cfg = AbrConfig {
            min_switch_interval: Duration::ZERO,
            min_buffer_for_up_switch_secs: 0.0,
            down_switch_buffer_secs: 0.0,
            mode: crate::options::AbrMode::Auto(Some(0)),
            ..AbrConfig::default()
        };

        let mut c = AbrController::new(cfg, None);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: 2_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
        });

        let d = c.decide(&variants(), 0.0, now);
        assert_eq!(d.target_variant_index, 2);
        assert!(d.changed);
        assert_eq!(d.reason, AbrReason::UpSwitch);
    }

    #[test]
    fn down_switch_when_buffer_low() {
        let cfg = AbrConfig {
            min_switch_interval: Duration::ZERO,
            mode: crate::options::AbrMode::Auto(Some(2)),
            ..AbrConfig::default()
        };

        let mut c = AbrController::new(cfg, None);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: 30_000,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
        });

        let d = c.decide(&variants(), 0.1, now);
        assert_eq!(d.target_variant_index, 0);
        assert!(d.changed);
        assert_eq!(d.reason, AbrReason::DownSwitch);
    }

    #[test]
    fn no_change_without_estimate() {
        let cfg = AbrConfig {
            mode: crate::options::AbrMode::Auto(Some(1)),
            ..AbrConfig::default()
        };
        let c = AbrController::new(cfg, None);
        let now = Instant::now();

        let d = c.decide(&variants(), 5.0, now);
        assert_eq!(d.target_variant_index, 1);
        assert!(!d.changed);
        assert_eq!(d.reason, AbrReason::NoEstimate);
    }
}
