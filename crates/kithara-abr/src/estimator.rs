#[cfg(test)]
use mockall::automock;

use super::{AbrOptions, ThroughputSample, ThroughputSampleSource};

/// Trait for throughput estimation strategies.
///
/// Allows testing `AbrController` with mock estimators.
#[cfg_attr(test, automock)]
pub trait Estimator {
    /// Get estimated throughput in bits per second.
    fn estimate_bps(&self) -> Option<u64>;

    /// Push a new throughput sample for estimation.
    fn push_sample(&mut self, sample: ThroughputSample);

    /// Get buffer level in seconds (total buffered content duration).
    fn buffer_level_secs(&self) -> f64;

    /// Reset buffer level.
    fn reset_buffer(&mut self);
}

#[derive(Clone, Debug)]
pub struct ThroughputEstimator {
    fast_ewma: Ewma,
    slow_ewma: Ewma,
    bytes_sampled: u64,
    initial_bps: f64,
    buffered_content_secs: f64,
}

impl ThroughputEstimator {
    const FAST_HALF_LIFE_SECS: f64 = 2.0;
    const SLOW_HALF_LIFE_SECS: f64 = 10.0;
    const MIN_CHUNK_BYTES: u64 = 16_000;
    const MIN_DURATION_MS: f64 = 0.5;

    pub fn new(_cfg: &AbrOptions) -> Self {
        Self {
            fast_ewma: Ewma::new(Self::FAST_HALF_LIFE_SECS),
            slow_ewma: Ewma::new(Self::SLOW_HALF_LIFE_SECS),
            bytes_sampled: 0,
            initial_bps: 0.0,
            buffered_content_secs: 0.0,
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
        // Accumulate buffered content duration
        if let Some(content_duration) = sample.content_duration {
            self.buffered_content_secs += content_duration.as_secs_f64();
        }

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

    pub fn buffer_level_secs(&self) -> f64 {
        self.buffered_content_secs
    }

    pub fn reset_buffer(&mut self) {
        self.buffered_content_secs = 0.0;
    }
}

impl Estimator for ThroughputEstimator {
    fn estimate_bps(&self) -> Option<u64> {
        self.estimate_bps()
    }

    fn push_sample(&mut self, sample: ThroughputSample) {
        self.push_sample(sample);
    }

    fn buffer_level_secs(&self) -> f64 {
        self.buffer_level_secs()
    }

    fn reset_buffer(&mut self) {
        self.reset_buffer();
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

    use rstest::rstest;

    use super::*;

    #[test]
    fn cache_hit_does_not_affect_throughput() {
        let cfg = AbrOptions::default();
        let mut est = ThroughputEstimator::new(&cfg);

        est.push_sample(ThroughputSample {
            bytes: 1000,
            duration: Duration::from_millis(100),
            at: Instant::now(),
            source: ThroughputSampleSource::Cache,
            content_duration: None,
        });

        assert_eq!(est.estimate_bps(), None);
    }

    #[rstest]
    #[case(vec![(500_000, 1000)], 3_500_000, "Single sample")]
    #[case(vec![(500_000, 1000), (500_000, 1000)], 3_800_000, "Stable throughput")]
    #[case(vec![(1_000_000, 1000), (1_000_000, 1000), (1_000_000, 1000)], 7_500_000, "Multiple stable samples")]
    fn test_ewma_estimation(
        #[case] samples: Vec<(u64, u64)>,
        #[case] expected_min_bps: u64,
        #[case] _description: &str,
    ) {
        let cfg = AbrOptions::default();
        let mut est = ThroughputEstimator::new(&cfg);
        let now = Instant::now();

        for (bytes, duration_ms) in samples {
            est.push_sample(ThroughputSample {
                bytes,
                duration: Duration::from_millis(duration_ms),
                at: now,
                source: ThroughputSampleSource::Network,
                content_duration: None,
            });
        }

        let estimate = est.estimate_bps();
        assert!(
            estimate.is_some(),
            "Should have estimate after network samples"
        );
        assert!(
            estimate.unwrap() >= expected_min_bps,
            "Estimate should be at least {expected_min_bps}"
        );
    }

    #[test]
    fn test_min_chunk_size_filtering() {
        let cfg = AbrOptions::default();
        let mut est = ThroughputEstimator::new(&cfg);
        let now = Instant::now();

        // Too small (< 16_000 bytes)
        est.push_sample(ThroughputSample {
            bytes: 10_000,
            duration: Duration::from_millis(100),
            at: now,
            source: ThroughputSampleSource::Network,
            content_duration: None,
        });

        assert_eq!(est.estimate_bps(), None, "Small chunks should be ignored");

        // Large enough
        est.push_sample(ThroughputSample {
            bytes: 100_000,
            duration: Duration::from_secs(1),
            at: now,
            source: ThroughputSampleSource::Network,
            content_duration: None,
        });

        assert!(
            est.estimate_bps().is_some(),
            "Large chunks should be counted"
        );
    }

    #[test]
    fn test_min_duration_clamping() {
        let cfg = AbrOptions::default();
        let mut est = ThroughputEstimator::new(&cfg);
        let now = Instant::now();

        // Very short duration (gets clamped to MIN_DURATION_MS)
        est.push_sample(ThroughputSample {
            bytes: 100_000,
            duration: Duration::from_nanos(1),
            at: now,
            source: ThroughputSampleSource::Network,
            content_duration: None,
        });

        let estimate = est.estimate_bps();
        assert!(estimate.is_some(), "Should still produce estimate");
        // With clamped duration, throughput should be very high
        assert!(
            estimate.unwrap() > 1_000_000,
            "Clamped duration should yield high throughput"
        );
    }

    #[test]
    fn test_no_estimate_without_samples() {
        let cfg = AbrOptions::default();
        let est = ThroughputEstimator::new(&cfg);

        assert_eq!(est.estimate_bps(), None, "No estimate without samples");
    }
}
