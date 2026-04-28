use kithara_events::BandwidthSource;
use kithara_platform::{Mutex, time::Duration};
#[cfg(any(test, feature = "internal"))]
use unimock::unimock;

#[derive(Clone, Debug)]
struct Ewma {
    alpha: f64,
    last_estimate: f64,
    total_weight: f64,
}

impl Ewma {
    const HALF_LIFE_BASE: f64 = 0.5;
    const MIN_HALF_LIFE_SECS: f64 = 0.001;
    const MIN_ZERO_FACTOR: f64 = 1e-6;

    fn new(half_life_secs: f64) -> Self {
        Self {
            alpha: f64::exp(
                Self::HALF_LIFE_BASE.ln() / half_life_secs.max(Self::MIN_HALF_LIFE_SECS),
            ),
            last_estimate: 0.0,
            total_weight: 0.0,
        }
    }

    fn add_sample(&mut self, weight: f64, val: f64) {
        let adj_alpha = self.alpha.powf(weight.max(0.0));
        self.last_estimate = val * (1.0 - adj_alpha) + adj_alpha * self.last_estimate;
        self.total_weight += weight.max(0.0);
    }

    fn get_estimate(&self) -> f64 {
        if self.total_weight <= 0.0 {
            0.0
        } else {
            let zero_factor = 1.0 - self.alpha.powf(self.total_weight);
            self.last_estimate / zero_factor.max(Self::MIN_ZERO_FACTOR)
        }
    }
}

/// Trait for throughput estimation strategies.
///
/// Uses interior mutability so that the controller can hold an
/// `Arc<dyn Estimator>` and share it across tick invocations.
#[cfg_attr(any(test, feature = "internal"), unimock(api = EstimatorMock))]
pub trait Estimator: Send + Sync {
    /// Current bandwidth estimate in bits per second. `None` while no
    /// samples have been absorbed yet.
    fn estimate_bps(&self) -> Option<u64>;

    /// Absorb a raw bandwidth sample. `duration` is the elapsed wall-clock
    /// time spent on the fetch (not the media duration of the payload).
    fn push_sample(&self, bytes: u64, duration: Duration, source: BandwidthSource);
}

/// Default EWMA-based throughput estimator shared across peers.
pub struct ThroughputEstimator {
    inner: Mutex<ThroughputInner>,
}

struct ThroughputInner {
    fast_ewma: Ewma,
    slow_ewma: Ewma,
    initial_bps: f64,
    bytes_sampled: u64,
}

impl ThroughputEstimator {
    const BITS_PER_BYTE_MS: f64 = 8000.0;
    const CACHE_INITIAL_BPS: f64 = 100_000_000.0;
    const FAST_HALF_LIFE_SECS: f64 = 2.0;
    const MIN_CHUNK_BYTES: u64 = 16_000;
    const MIN_DURATION_MS: f64 = 0.5;
    const MS_PER_SEC: f64 = 1000.0;
    const SLOW_HALF_LIFE_SECS: f64 = 10.0;

    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(ThroughputInner {
                fast_ewma: Ewma::new(Self::FAST_HALF_LIFE_SECS),
                slow_ewma: Ewma::new(Self::SLOW_HALF_LIFE_SECS),
                initial_bps: 0.0,
                bytes_sampled: 0,
            }),
        }
    }
}

impl Default for ThroughputEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl Estimator for ThroughputEstimator {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn estimate_bps(&self) -> Option<u64> {
        let inner = self.inner.lock_sync();
        let est = inner
            .fast_ewma
            .get_estimate()
            .min(inner.slow_ewma.get_estimate());

        if est > 0.0 {
            Some(est.round() as u64)
        } else if inner.initial_bps > 0.0 {
            Some(inner.initial_bps.round() as u64)
        } else {
            None
        }
    }

    fn push_sample(&self, bytes: u64, duration: Duration, source: BandwidthSource) {
        let mut inner = self.inner.lock_sync();

        if matches!(source, BandwidthSource::Cache) {
            inner.initial_bps = Self::CACHE_INITIAL_BPS;
            return;
        }

        if bytes < Self::MIN_CHUNK_BYTES {
            return;
        }

        let dur_ms = (duration.as_secs_f64() * Self::MS_PER_SEC).max(Self::MIN_DURATION_MS);
        #[expect(clippy::cast_precision_loss)]
        let bps = (bytes as f64) * Self::BITS_PER_BYTE_MS / dur_ms;
        let weight_secs = dur_ms / Self::MS_PER_SEC;

        inner.fast_ewma.add_sample(weight_secs, bps);
        inner.slow_ewma.add_sample(weight_secs, bps);
        inner.bytes_sampled = inner.bytes_sampled.saturating_add(bytes);
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::BandwidthSource;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn cache_hit_sets_high_initial_bps() {
        let est = ThroughputEstimator::new();
        assert_eq!(est.estimate_bps(), None);

        est.push_sample(1000, Duration::from_millis(100), BandwidthSource::Cache);
        assert_eq!(est.estimate_bps(), Some(100_000_000));
    }

    #[kithara::test(wasm)]
    #[case(vec![(500_000, 1000)], 3_500_000)]
    #[case(vec![(500_000, 1000), (500_000, 1000)], 3_800_000)]
    #[case(vec![(1_000_000, 1000), (1_000_000, 1000), (1_000_000, 1000)], 7_500_000)]
    fn ewma_estimation(#[case] samples: Vec<(u64, u64)>, #[case] expected_min_bps: u64) {
        let est = ThroughputEstimator::new();
        for (bytes, duration_ms) in samples {
            est.push_sample(
                bytes,
                Duration::from_millis(duration_ms),
                BandwidthSource::Network,
            );
        }
        let estimate = est.estimate_bps();
        assert!(estimate.is_some());
        assert!(estimate.unwrap_or(0) >= expected_min_bps);
    }

    #[kithara::test]
    fn min_chunk_size_filtering() {
        let est = ThroughputEstimator::new();
        est.push_sample(10_000, Duration::from_millis(100), BandwidthSource::Network);
        assert_eq!(est.estimate_bps(), None);

        est.push_sample(100_000, Duration::from_secs(1), BandwidthSource::Network);
        assert!(est.estimate_bps().is_some());
    }

    #[kithara::test]
    fn min_duration_clamping() {
        let est = ThroughputEstimator::new();
        est.push_sample(100_000, Duration::from_nanos(1), BandwidthSource::Network);
        let estimate = est.estimate_bps();
        assert!(estimate.is_some());
        assert!(estimate.unwrap_or(0) > 1_000_000);
    }

    #[kithara::test]
    fn no_estimate_without_samples() {
        let est = ThroughputEstimator::new();
        assert_eq!(est.estimate_bps(), None);
    }
}
