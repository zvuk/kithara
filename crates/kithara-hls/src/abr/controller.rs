use std::time::Instant;

use hls_m3u8::MasterPlaylist;

use super::{AbrConfig, ThroughputEstimator, ThroughputSample, Variant, VariantSelector};

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

pub struct AbrController {
    cfg: AbrConfig,
    variant_selector: Option<VariantSelector>,
    estimator: ThroughputEstimator,
    current_variant_index: usize,
    last_switch_at: Option<Instant>,
}

impl AbrController {
    pub fn new(
        cfg: AbrConfig,
        variant_selector: Option<VariantSelector>,
        initial_variant: usize,
    ) -> Self {
        let estimator = ThroughputEstimator::new(&cfg);
        Self {
            cfg,
            variant_selector,
            estimator,
            current_variant_index: initial_variant,
            last_switch_at: None,
        }
    }

    pub fn current_variant(&self) -> usize {
        self.current_variant_index
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
        let current = self.current_variant_index;

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

        let effective_bps = (estimate_bps as f64 / self.cfg.throughput_safety_factor).max(0.0);

        let current_bw = variants
            .iter()
            .find(|v| v.variant_index == current)
            .map(|v| v.bandwidth_bps)
            .unwrap_or(0);

        let mut by_bw: Vec<Variant> = variants.to_vec();
        by_bw.sort_by_key(|v| v.bandwidth_bps);

        if buffer_level_secs >= self.cfg.min_buffer_for_up_switch_secs {
            if let Some(candidate) = by_bw
                .iter()
                .rev()
                .find(|v| (v.bandwidth_bps as f64) * self.cfg.up_hysteresis_ratio <= effective_bps)
            {
                if candidate.bandwidth_bps > current_bw {
                    return AbrDecision {
                        target_variant_index: candidate.variant_index,
                        reason: AbrReason::UpSwitch,
                        changed: true,
                    };
                }
            }
        } else {
            let up_candidate_exists = by_bw.iter().any(|v| {
                (v.bandwidth_bps as f64) * self.cfg.up_hysteresis_ratio <= effective_bps
                    && v.bandwidth_bps > current_bw
            });
            if up_candidate_exists {
                return AbrDecision {
                    target_variant_index: current,
                    reason: AbrReason::BufferTooLowForUpSwitch,
                    changed: false,
                };
            }
        }

        let down_switch_condition = effective_bps
            < (current_bw as f64) * self.cfg.down_hysteresis_ratio
            || buffer_level_secs <= self.cfg.down_switch_buffer_secs;

        if down_switch_condition {
            let candidate = by_bw
                .iter()
                .rev()
                .filter(|v| v.variant_index != current)
                .find(|v| (v.bandwidth_bps as f64) <= effective_bps)
                .or_else(|| by_bw.first())
                .map(|v| v.variant_index);

            if let Some(target) = candidate {
                if target != current {
                    return AbrDecision {
                        target_variant_index: target,
                        reason: AbrReason::DownSwitch,
                        changed: true,
                    };
                }
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
        master_playlist: &MasterPlaylist<'_>,
        variants: &[Variant],
        buffer_level_secs: f64,
        now: Instant,
    ) -> AbrDecision {
        if let Some(selector) = self.variant_selector.as_ref() {
            if let Some(manual) = selector(master_playlist) {
                return AbrDecision {
                    target_variant_index: manual,
                    reason: AbrReason::ManualOverride,
                    changed: manual != self.current_variant_index,
                };
            }
        }

        self.decide(variants, buffer_level_secs, now)
    }

    pub fn apply(&mut self, decision: &AbrDecision, now: Instant) {
        if decision.target_variant_index == self.current_variant_index {
            return;
        }
        self.current_variant_index = decision.target_variant_index;
        self.last_switch_at = Some(now);
    }

    fn can_switch_now(&self, now: Instant) -> bool {
        self.last_switch_at
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
        let mut cfg = AbrConfig::default();
        cfg.throughput_safety_factor = 1.5;
        cfg.min_buffer_for_up_switch_secs = 0.0;
        cfg.down_switch_buffer_secs = 0.0;
        cfg.min_switch_interval = Duration::ZERO;

        let mut c = AbrController::new(cfg, None, 2);
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
        let mut cfg = AbrConfig::default();
        cfg.min_buffer_for_up_switch_secs = 10.0;
        cfg.throughput_safety_factor = 1.5;
        cfg.up_hysteresis_ratio = 1.3;
        cfg.min_switch_interval = Duration::ZERO;

        let mut c = AbrController::new(cfg, None, 0);
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
        let mut cfg = AbrConfig::default();
        cfg.min_switch_interval = Duration::from_secs(30);
        cfg.min_buffer_for_up_switch_secs = 0.0;
        cfg.down_switch_buffer_secs = 0.0;

        let mut c = AbrController::new(cfg, None, 1);
        let now = Instant::now();
        c.push_throughput_sample(ThroughputSample {
            bytes: 2_000_000 / 8,
            duration: Duration::from_secs(1),
            at: now,
            source: super::super::ThroughputSampleSource::Network,
        });

        let d = c.decide(&variants(), 20.0, now);
        assert!(d.changed);
        c.apply(&d, now);

        let suppressed = c.decide(&variants(), 20.0, now + Duration::from_secs(1));
        assert!(!suppressed.changed);
        assert_eq!(suppressed.reason, AbrReason::MinInterval);
    }
}
