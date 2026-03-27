#[cfg(test)]
use unimock::unimock;

use super::{AbrOptions, ThroughputSample, ThroughputSampleSource};
use crate::ewma::Ewma;

/// Trait for throughput estimation strategies.
///
/// Allows testing `AbrController` with mock estimators.
#[cfg_attr(test, unimock(api = EstimatorMock))]
pub trait Estimator {
    fn estimate_bps(&self) -> Option<u64>;
    fn push_sample(&mut self, sample: ThroughputSample);
    fn buffer_level_secs(&self) -> f64;
    fn reset_buffer(&mut self);
}

#[derive(Clone, Debug)]
pub struct ThroughputEstimator {
    buffered_content_secs: f64,
    bytes_sampled: u64,
    fast_ewma: Ewma,
    initial_bps: f64,
    slow_ewma: Ewma,
}

impl ThroughputEstimator {
    const FAST_HALF_LIFE_SECS: f64 = 2.0;
    const SLOW_HALF_LIFE_SECS: f64 = 10.0;
    const MIN_CHUNK_BYTES: u64 = 16_000;
    const MIN_DURATION_MS: f64 = 0.5;
    const MS_PER_SEC: f64 = 1000.0;
    const BITS_PER_BYTE_MS: f64 = 8000.0;
    const CACHE_INITIAL_BPS: f64 = 100_000_000.0;

    #[must_use]
    pub fn new(_cfg: &AbrOptions) -> Self {
        Self {
            buffered_content_secs: 0.0,
            bytes_sampled: 0,
            fast_ewma: Ewma::new(Self::FAST_HALF_LIFE_SECS),
            initial_bps: 0.0,
            slow_ewma: Ewma::new(Self::SLOW_HALF_LIFE_SECS),
        }
    }

    #[must_use]
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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
        if let Some(content_duration) = sample.content_duration {
            self.buffered_content_secs += content_duration.as_secs_f64();
        }

        if matches!(sample.source, ThroughputSampleSource::Cache) {
            self.initial_bps = Self::CACHE_INITIAL_BPS;
            return;
        }

        if sample.bytes < Self::MIN_CHUNK_BYTES {
            return;
        }

        let dur_ms = (sample.duration.as_secs_f64() * Self::MS_PER_SEC).max(Self::MIN_DURATION_MS);
        #[expect(clippy::cast_precision_loss)]
        let bps = (sample.bytes as f64) * Self::BITS_PER_BYTE_MS / dur_ms;
        let weight_secs = dur_ms / Self::MS_PER_SEC;

        self.fast_ewma.add_sample(weight_secs, bps);
        self.slow_ewma.add_sample(weight_secs, bps);
        self.bytes_sampled = self.bytes_sampled.saturating_add(sample.bytes);
    }

    #[must_use]
    pub fn buffer_level_secs(&self) -> f64 {
        self.buffered_content_secs
    }

    pub fn reset_buffer(&mut self) {
        self.buffered_content_secs = 0.0;
    }
}

impl Estimator for ThroughputEstimator {
    delegate::delegate! {
        to self {
            fn estimate_bps(&self) -> Option<u64>;
            fn push_sample(&mut self, sample: ThroughputSample);
            fn buffer_level_secs(&self) -> f64;
            fn reset_buffer(&mut self);
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn cache_hit_sets_high_initial_bps() {
        let mut est = ThroughputEstimator::new(&AbrOptions::default());
        assert_eq!(est.estimate_bps(), None);

        est.push_sample(ThroughputSample {
            bytes: 1000,
            duration: Duration::from_millis(100),
            at: Instant::now(),
            source: ThroughputSampleSource::Cache,
            content_duration: None,
        });
        assert_eq!(est.estimate_bps(), Some(100_000_000));
    }

    #[kithara::test(wasm)]
    #[case(vec![(500_000, 1000)], 3_500_000)]
    #[case(vec![(500_000, 1000), (500_000, 1000)], 3_800_000)]
    #[case(vec![(1_000_000, 1000), (1_000_000, 1000), (1_000_000, 1000)], 7_500_000)]
    fn test_ewma_estimation(#[case] samples: Vec<(u64, u64)>, #[case] expected_min_bps: u64) {
        let mut est = ThroughputEstimator::new(&AbrOptions::default());
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
        assert!(estimate.is_some());
        assert!(estimate.unwrap_or(0) >= expected_min_bps);
    }

    #[kithara::test]
    fn test_min_chunk_size_filtering() {
        let mut est = ThroughputEstimator::new(&AbrOptions::default());
        let now = Instant::now();
        est.push_sample(ThroughputSample {
            bytes: 10_000,
            duration: Duration::from_millis(100),
            at: now,
            source: ThroughputSampleSource::Network,
            content_duration: None,
        });
        assert_eq!(est.estimate_bps(), None);

        est.push_sample(ThroughputSample {
            bytes: 100_000,
            duration: Duration::from_secs(1),
            at: now,
            source: ThroughputSampleSource::Network,
            content_duration: None,
        });
        assert!(est.estimate_bps().is_some());
    }

    #[kithara::test]
    fn test_min_duration_clamping() {
        let mut est = ThroughputEstimator::new(&AbrOptions::default());
        est.push_sample(ThroughputSample {
            bytes: 100_000,
            duration: Duration::from_nanos(1),
            at: Instant::now(),
            source: ThroughputSampleSource::Network,
            content_duration: None,
        });
        let estimate = est.estimate_bps();
        assert!(estimate.is_some());
        assert!(estimate.unwrap_or(0) > 1_000_000);
    }

    #[kithara::test]
    fn test_no_estimate_without_samples() {
        let est = ThroughputEstimator::new(&AbrOptions::default());
        assert_eq!(est.estimate_bps(), None);
    }
}
