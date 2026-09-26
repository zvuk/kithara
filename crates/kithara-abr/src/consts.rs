use kithara_platform::time::Duration;

pub(crate) const BANDWIDTH_EMIT_MIN_DELTA_RATIO: f64 = 0.10;
pub(crate) const BANDWIDTH_EMIT_MIN_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const BUFFER_EMIT_MIN_DELTA: Duration = Duration::from_millis(500);
pub(crate) const BUFFER_EMIT_MIN_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const DOWN_HYSTERESIS_RATIO: f64 = 0.8;
pub(crate) const INITIAL_THROUGHPUT_BPS: u64 = 2_000_000;
pub(crate) const MIN_BUFFER_FOR_UP_SWITCH: Duration = Duration::from_secs(10);
pub(crate) const MIN_SWITCH_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const THROUGHPUT_SAFETY_FACTOR: f64 = 1.5;
pub(crate) const THROUGHPUT_SAMPLE_MIN_INTERVAL: Duration = Duration::from_millis(200);
pub(crate) const UP_HYSTERESIS_RATIO: f64 = 1.3;
pub(crate) const URGENT_DOWNSWITCH_BUFFER: Duration = Duration::from_secs(5);

/// Threshold separating Manual (below) from Auto (at or above) in the packed
/// `usize` representation of [`AbrMode`].
pub(crate) const ABR_MODE_AUTO_THRESHOLD: usize = usize::MAX / 2;
