use std::time::{Duration, Instant};

use super::{AbrConfig, ThroughputSample, ThroughputSampleSource};

#[derive(Clone, Debug)]
pub struct ThroughputEstimator {
    sample_window: Duration,
    alpha: f64,
    last_update_at: Option<Instant>,
    estimate_bps: Option<f64>,
}

impl ThroughputEstimator {
    pub fn new(cfg: &AbrConfig) -> Self {
        Self {
            sample_window: cfg.sample_window,
            alpha: 0.3,
            last_update_at: None,
            estimate_bps: None,
        }
    }

    pub fn estimate_bps(&self) -> Option<u64> {
        self.estimate_bps.map(|v| v.round() as u64)
    }

    pub fn push_sample(&mut self, sample: ThroughputSample) {
        if !matches!(sample.source, ThroughputSampleSource::Network) {
            return;
        }
        if sample.duration == Duration::ZERO || sample.bytes == 0 {
            return;
        }

        if let Some(last_update_at) = self.last_update_at {
            if sample.at.duration_since(last_update_at) > self.sample_window {
                self.estimate_bps = None;
            }
        }

        let sample_bps = (sample.bytes as f64 * 8.0) / sample.duration.as_secs_f64();
        self.estimate_bps = Some(match self.estimate_bps {
            None => sample_bps,
            Some(prev) => self.alpha * sample_bps + (1.0 - self.alpha) * prev,
        });
        self.last_update_at = Some(sample.at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_does_not_affect_throughput() {
        let cfg = AbrConfig::default();
        let mut est = ThroughputEstimator::new(&cfg);

        est.push_sample(ThroughputSample {
            bytes: 1000,
            duration: Duration::from_millis(100),
            at: Instant::now(),
            source: ThroughputSampleSource::Cache,
        });

        assert_eq!(est.estimate_bps(), None);
    }
}
