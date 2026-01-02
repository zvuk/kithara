use super::ewma::Ewma;

/// Bandwidth estimator based on two EWMAs (fast + slow).
///
/// The estimator tracks a fast-moving and a slow-moving average and uses the minimum of the two,
/// so bandwidth drops affect decisions quickly while increases require sustained evidence.
#[derive(Debug)]
pub struct BandwidthEstimator {
    fast_ewma: Ewma,
    slow_ewma: Ewma,
    bytes_sampled: u64,
    initial_bandwidth: f64,
}

impl BandwidthEstimator {
    const FAST_EWMA_HALF_LIFE: u32 = 2;
    const SLOW_EWMA_HALF_LIFE: u32 = 10;
    const MINIMUM_CHUNK_SIZE: u32 = 16_000;
    const MINIMUM_TOTAL_BYTES: u64 = 150_000;

    /// Creates a new `BandwidthEstimator`.
    pub(crate) fn new(initial_bandwidth: f64) -> Self {
        Self {
            fast_ewma: Ewma::new(Self::FAST_EWMA_HALF_LIFE),
            slow_ewma: Ewma::new(Self::SLOW_EWMA_HALF_LIFE),
            bytes_sampled: 0,
            initial_bandwidth,
        }
    }

    /// Adds a download observation.
    ///
    /// `duration_ms` is the download duration (milliseconds) and `size_bytes` is the payload size.
    /// Very small samples are ignored to reduce noise.
    pub(crate) fn add_sample(&mut self, duration_ms: f64, size_bytes: u32) {
        if size_bytes < Self::MINIMUM_CHUNK_SIZE {
            return;
        }
        let bandwidth = (size_bytes as f64) * 8000. / duration_ms;
        let weight = duration_ms / 1000.;
        self.bytes_sampled += size_bytes as u64;
        self.fast_ewma.add_sample(weight, bandwidth);
        self.slow_ewma.add_sample(weight, bandwidth);
    }

    /// Returns the current bandwidth estimate (bits per second).
    ///
    /// Until enough data is collected, this returns `initial_bandwidth`.
    pub(crate) fn get_estimate(&self) -> f64 {
        if self.bytes_sampled < Self::MINIMUM_TOTAL_BYTES {
            self.initial_bandwidth
        } else {
            self.fast_ewma
                .get_estimate()
                .min(self.slow_ewma.get_estimate())
        }
    }
}
