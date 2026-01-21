use std::time::Duration;

/// Statistics for a segment download.
#[derive(Debug, Clone, Copy)]
pub struct DownloadStats {
    /// Number of bytes downloaded.
    pub bytes: u64,

    /// Time taken to download.
    pub duration: Duration,

    /// Estimated throughput in bits per second.
    pub throughput_bps: f64,
}

/// Tracks throughput across multiple segment downloads.
///
/// Accumulates bytes and duration to provide a moving average
/// of download throughput for ABR decisions.
#[derive(Debug, Clone)]
pub struct ThroughputAccumulator {
    accumulated_bytes: u64,
    accumulated_duration: Duration,
}

impl ThroughputAccumulator {
    /// Create a new accumulator.
    pub fn new() -> Self {
        Self {
            accumulated_bytes: 0,
            accumulated_duration: Duration::ZERO,
        }
    }

    /// Record a segment download and return statistics.
    pub fn record_segment(&mut self, bytes: u64, duration: Duration) -> DownloadStats {
        self.accumulated_bytes += bytes;
        self.accumulated_duration += duration;

        let throughput_bps = if self.accumulated_duration.as_secs_f64() > 0.0 {
            (self.accumulated_bytes as f64 * 8.0) / self.accumulated_duration.as_secs_f64()
        } else {
            0.0
        };

        DownloadStats {
            bytes,
            duration,
            throughput_bps,
        }
    }

    /// Reset the accumulator.
    pub fn reset(&mut self) {
        self.accumulated_bytes = 0;
        self.accumulated_duration = Duration::ZERO;
    }

    /// Get current accumulated bytes.
    pub fn accumulated_bytes(&self) -> u64 {
        self.accumulated_bytes
    }

    /// Get current accumulated duration.
    pub fn accumulated_duration(&self) -> Duration {
        self.accumulated_duration
    }

    /// Get current throughput estimate (bits per second).
    pub fn throughput_bps(&self) -> f64 {
        if self.accumulated_duration.as_secs_f64() > 0.0 {
            (self.accumulated_bytes as f64 * 8.0) / self.accumulated_duration.as_secs_f64()
        } else {
            0.0
        }
    }
}

impl Default for ThroughputAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let acc = ThroughputAccumulator::new();
        assert_eq!(acc.accumulated_bytes(), 0);
        assert_eq!(acc.accumulated_duration(), Duration::ZERO);
        assert_eq!(acc.throughput_bps(), 0.0);
    }

    #[test]
    fn test_record_single_segment() {
        let mut acc = ThroughputAccumulator::new();

        let stats = acc.record_segment(1000, Duration::from_secs(1));

        assert_eq!(stats.bytes, 1000);
        assert_eq!(stats.duration, Duration::from_secs(1));
        assert_eq!(stats.throughput_bps, 8000.0);

        assert_eq!(acc.accumulated_bytes(), 1000);
        assert_eq!(acc.accumulated_duration(), Duration::from_secs(1));
        assert_eq!(acc.throughput_bps(), 8000.0);
    }

    #[test]
    fn test_record_multiple_segments() {
        let mut acc = ThroughputAccumulator::new();

        acc.record_segment(1000, Duration::from_secs(1));
        let stats = acc.record_segment(2000, Duration::from_secs(1));

        assert_eq!(stats.bytes, 2000);
        assert_eq!(acc.accumulated_bytes(), 3000);
        assert_eq!(acc.accumulated_duration(), Duration::from_secs(2));
        assert_eq!(acc.throughput_bps(), 12000.0);
    }

    #[test]
    fn test_reset() {
        let mut acc = ThroughputAccumulator::new();

        acc.record_segment(1000, Duration::from_secs(1));
        acc.reset();

        assert_eq!(acc.accumulated_bytes(), 0);
        assert_eq!(acc.accumulated_duration(), Duration::ZERO);
        assert_eq!(acc.throughput_bps(), 0.0);
    }

    #[test]
    fn test_zero_duration_edge_case() {
        let mut acc = ThroughputAccumulator::new();

        let stats = acc.record_segment(1000, Duration::ZERO);

        assert_eq!(stats.throughput_bps, 0.0);
        assert_eq!(acc.throughput_bps(), 0.0);
    }

    #[test]
    fn test_default_impl() {
        let acc = ThroughputAccumulator::default();
        assert_eq!(acc.accumulated_bytes(), 0);
        assert_eq!(acc.accumulated_duration(), Duration::ZERO);
    }
}
