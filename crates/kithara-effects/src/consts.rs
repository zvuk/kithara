use core::num::NonZeroUsize;

use kithara_dsp::param::{MIN_SETTLE_RATIO, SmootherConfig};

pub(crate) const EQ_BLOCK_FRAMES: usize = 256;
/// Highest cutoff the deck filter knob map gives, as a share of the sample
/// rate: below Nyquist with margin, and above every default cutoff at 44.1 kHz
/// and up.
pub(crate) const NYQUIST_SHARE: f64 = 0.45;

/// Frames of stack scratch per feedback-comb channel.
pub(crate) const COMB_CHUNK: usize = 256;
/// The -120 dBFS write floor used by the feedback-comb tail policy.
pub(crate) const COMB_FLOOR: f32 = 1e-6;

/// Seconds a delay time change crosses from the old read to the new one.
pub(crate) const DELAY_RAMP_SECONDS: f64 = 0.02;

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

/// Milliseconds per second: the release time arrives in ms, the coefficient
/// is computed in samples.
pub(crate) const MS_PER_SEC: f32 = 1000.0;

/// Channels the limiter node and the deck filter carry.
pub(crate) const STEREO: NonZeroUsize = NonZeroUsize::new(2).expect("invariant: 2 is non-zero");

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

/// The rate the stereo EQ tests build their layouts at.
#[cfg(test)]
pub(crate) const TEST_RATE: core::num::NonZeroU32 =
    core::num::NonZeroU32::new(48_000).expect("a non-zero rate");

pub(crate) const REVERB_COMB_LENGTHS: [u32; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
pub(crate) const REVERB_DIFFUSER_LENGTHS: [u32; 4] = [31, 47, 73, 101];
pub(crate) const REVERB_COMBS: usize = REVERB_COMB_LENGTHS.len();
pub(crate) const REVERB_SCRATCH: usize = 128;
pub(crate) const REVERB_REFERENCE_RATE: f64 = 44_100.0;
pub(crate) const REVERB_STEREO_SPREAD: u32 = 13;
pub(crate) const REVERB_DIFFUSION: f32 = 0.5;
pub(crate) const REVERB_DAMPING_SCALE: f64 = 0.4;
pub(crate) const REVERB_TAIL_RT60_MULTIPLIER: f64 = 2.0;
pub(crate) const REVERB_TAIL_THRESHOLD: f64 = 1e-6;
