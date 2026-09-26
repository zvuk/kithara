/// Analysis window, 46.4 ms.
pub(super) const FRAMES_FRAME: usize = 1024;

/// Hop, 11.61 ms: the detection-function resolution the papers fix.
pub(super) const FRAMES_HOP: usize = 256;

/// The rate the crate contract fixes.
pub(super) const FRAMES_RATE: f32 = 22_050.0;

pub(super) const NOVELTY_HANN_A0: f32 = 0.5;

/// Analysis stride, 23.2 ms: the rate the difference is actually
/// measured at.
pub(super) const NOVELTY_STRIDE: usize = 2 * FRAMES_HOP;

/// Fastest tempo the detector tracks.
pub(super) const TEMPO_BAND_HIGH_BPM: f32 = 185.0;

/// Slowest tempo the detector tracks.
pub(super) const TEMPO_BAND_LOW_BPM: f32 = 48.0;

/// The tempo the periodicity stage prefers inside the band.
pub(super) const TEMPO_PRIOR_BPM: f32 = 120.0;

/// Deviation allowed between consecutive beats, in seconds.
pub(super) const TEMPO_TOLERANCE_SECONDS: f32 = 0.025;

/// Periodicity window, 512 detection-function frames (5.94 s).
pub(super) const PERIOD_ACF_FRAME: usize = 512;

/// One beat-period estimate every 128 frames (1.49 s), a 75% overlap.
pub(super) const PERIOD_ACF_STEP: usize = 128;

/// Comb elements each hypothesis is scored over.
pub(super) const PERIOD_COMB_HARMONICS: usize = 4;

/// Hypothesis `i` is a period of `i + 1` lags; one per possible lag up
/// to the estimate spacing.
pub(super) const PERIOD_HYPOTHESES: usize = PERIOD_ACF_STEP;

/// Widest comb element reaches 3 lags below its harmonic, and the top
/// hypothesis is where its widest element still reads inside the window.
pub(super) const COMB_PERIOD_INDEX: std::ops::RangeInclusive<usize> = (PERIOD_COMB_HARMONICS - 1)
    ..=((PERIOD_ACF_FRAME - (PERIOD_COMB_HARMONICS - 1)) / PERIOD_COMB_HARMONICS - 1);

/// Adaptive-threshold half window, 0.1 s of detection frames.
pub(super) const PERIOD_SMOOTH_HALF: usize = 8;

/// Between-estimate spread of the period at the default drift, in lags.
pub(super) const PERIOD_TRANSITION_SIGMA: f32 = 8.0;

/// The Gaussian transition's support, in standard deviations.
pub(super) const PERIOD_TRANSITION_SUPPORT_SIGMAS: f32 = 4.0;

/// Height scale of the interval density: the Gaussian claims about 0.43
/// of each state's transition mass, keeping every beat transition soft.
pub(super) const DECODE_DENSITY_SCALE: f32 = 0.005;

pub(super) const DECODE_EPSILON: f32 = 1e-6;

/// Observations top out below one, so a skipped peak stays payable.
pub(super) const DECODE_OBSERVED_CEILING: f32 = 0.99;

/// How far past the longest period the state space reaches, in standard
/// deviations: the longest wait the decoder can express.
pub(super) const DECODE_STATE_MARGIN: f32 = 3.0;

/// The interval density's support, in standard deviations.
pub(super) const DECODE_SUPPORT: f32 = 4.0;

/// A mark is never a certainty, and never nothing.
pub(super) const TRACKER_CONFIDENCE_BOUNDS: (f32, f32) = (0.001, 0.999);
