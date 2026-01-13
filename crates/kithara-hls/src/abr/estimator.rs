use super::{AbrConfig, ThroughputSample, ThroughputSampleSource};

#[derive(Clone, Debug)]
pub struct ThroughputEstimator {
    fast_ewma: Ewma,
    slow_ewma: Ewma,
    bytes_sampled: u64,
    initial_bps: f64,
}

impl ThroughputEstimator {
    const FAST_HALF_LIFE_SECS: f64 = 2.0;
    const SLOW_HALF_LIFE_SECS: f64 = 10.0;
    const MIN_CHUNK_BYTES: u64 = 16_000;
    const MIN_DURATION_MS: f64 = 0.5;

    pub fn new(_cfg: &AbrConfig) -> Self {
        Self {
            fast_ewma: Ewma::new(Self::FAST_HALF_LIFE_SECS),
            slow_ewma: Ewma::new(Self::SLOW_HALF_LIFE_SECS),
            bytes_sampled: 0,
            initial_bps: 0.0,
        }
    }

    pub fn estimate_bps(&self) -> Option<u64> {
        let est = self
            .fast_ewma
            .get_estimate()
            .min(self.slow_ewma.get_estimate());

        if est > 0.0 {
            Some(est.round() as u64)
        } else if self.initial_bps > 0.0 {
            Some(self.initial_bps.round() as u64)
        } else {
            None
        }
    }

    pub fn push_sample(&mut self, sample: ThroughputSample) {
        if !matches!(sample.source, ThroughputSampleSource::Network) {
            return;
        }
        if sample.bytes < Self::MIN_CHUNK_BYTES {
            return;
        }

        let dur_ms = (sample.duration.as_secs_f64() * 1000.0).max(Self::MIN_DURATION_MS);
        let bps = (sample.bytes as f64) * 8000.0 / dur_ms;
        let weight_secs = dur_ms / 1000.0;

        self.fast_ewma.add_sample(weight_secs, bps);
        self.slow_ewma.add_sample(weight_secs, bps);
        self.bytes_sampled = self.bytes_sampled.saturating_add(sample.bytes);
    }
}

#[derive(Clone, Debug)]
struct Ewma {
    alpha: f64,
    last_estimate: f64,
    total_weight: f64,
}

impl Ewma {
    fn new(half_life_secs: f64) -> Self {
        Self {
            alpha: f64::exp(0.5_f64.ln() / half_life_secs.max(0.001)),
            last_estimate: 0.0,
            total_weight: 0.0,
        }
    }

    fn add_sample(&mut self, weight: f64, val: f64) {
        let adj_alpha = self.alpha.powf(weight.max(0.0));
        let new_estimate = val * (1.0 - adj_alpha) + adj_alpha * self.last_estimate;
        self.last_estimate = new_estimate;
        self.total_weight += weight.max(0.0);
    }

    fn get_estimate(&self) -> f64 {
        if self.total_weight <= 0.0 {
            0.0
        } else {
            let zero_factor = 1.0 - self.alpha.powf(self.total_weight);
            self.last_estimate / zero_factor.max(1e-6)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

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
