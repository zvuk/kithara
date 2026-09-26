use biquad::Coefficients;
use firewheel::{dsp::filter::smoothing_filter::MIN_SETTLE_RATIO, param::smoother::SmootherConfig};

pub(crate) const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;
pub(crate) const NYQUIST_FACTOR: f32 = 2.0;

pub(crate) const PASSTHROUGH: Coefficients<f32> = Coefficients {
    a1: 0.0,
    a2: 0.0,
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
};

pub(crate) const BAND_MAX_FREQ: f32 = 18000.0;
pub(crate) const BAND_MIN_FREQ: f32 = 60.0;

/// Centre frequency a band starts at before the caller places it.
pub(crate) const DEFAULT_FREQ: f32 = 1000.0;

pub(crate) const HIGH_SHELF_DISCRIMINANT: u8 = 2;
pub(crate) const LOG_FREQ_BASE: f32 = 10.0;
pub(crate) const Q_REFERENCE_BANDS: f32 = 10.0;
pub(crate) const Q_SCALE_FACTOR: f32 = 1.4;

pub(crate) const DEFAULT_EQ_SMOOTHING: SmootherConfig = SmootherConfig {
    smooth_seconds: 0.01,
    settle_ratio: MIN_SETTLE_RATIO,
};

pub(crate) const DB_DIVISOR: f32 = 20.0;
pub(crate) const DB_LOG_BASE: f32 = 10.0;
pub(crate) const DEFAULT_CEILING: f32 = 0.98;
pub(crate) const DEFAULT_RELEASE_MS: f32 = 50.0;

/// Milliseconds per second: the release time arrives in ms, the coefficient
/// is computed in samples.
pub(crate) const MS_PER_SEC: f32 = 1000.0;

#[cfg(test)]
pub(crate) const CEILING: f32 = 0.98;

/// How far a reconstructed level may sit from the ceiling. The detector's kernel is
/// shorter than a full reconstruction, and on the fixtures here the two disagree by
/// about two parts in ten thousand.
#[cfg(test)]
pub(crate) const DETECTOR_RESOLUTION: f32 = 1e-3;

/// How far above its samples a step out of silence reconstructs. A block that starts at
/// full level carries that overshoot, and the limiter has to duck by it; the fixtures
/// below start exactly that way, so their output lands a step overshoot under the
/// ceiling rather than on it.
#[cfg(test)]
pub(crate) const STEP_OVERSHOOT: f32 = 1.1351;
