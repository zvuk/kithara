pub(super) struct FramesConsts;

impl FramesConsts {
    /// The rate the crate contract fixes.
    pub(super) const RATE: f32 = 22_050.0;
    /// Analysis window, 46.4 ms.
    pub(super) const FRAME: usize = 1024;
    /// Hop, 11.61 ms: the detection-function resolution the papers fix.
    pub(super) const HOP: usize = 256;
    /// Deviation allowed between consecutive beats.
    pub(super) const SIGMA_SECONDS: f32 = 0.025;
}

pub(super) struct NoveltyConsts;

impl NoveltyConsts {
    pub(super) const HANN_A0: f32 = 0.5;
    /// Analysis stride, 23.2 ms: the rate the difference is actually
    /// measured at.
    pub(super) const STRIDE: usize = 2 * FramesConsts::HOP;
}

pub(super) struct TempoConsts;

impl TempoConsts {
    /// Fastest tempo the detector tracks.
    pub(super) const BAND_HIGH_BPM: f32 = 185.0;
    /// Slowest tempo the detector tracks.
    pub(super) const BAND_LOW_BPM: f32 = 48.0;
    /// The tempo the periodicity stage prefers inside the band.
    pub(super) const PRIOR_BPM: f32 = 120.0;
}

pub(super) struct PeriodConsts;

impl PeriodConsts {
    /// Periodicity window, 512 detection-function frames (5.94 s).
    pub(super) const ACF_FRAME: usize = 512;
    /// One beat-period estimate every 128 frames (1.49 s), a 75% overlap.
    pub(super) const ACF_STEP: usize = 128;
    /// Comb elements each hypothesis is scored over.
    pub(super) const COMB_HARMONICS: usize = 4;
    /// Hypothesis `i` is a period of `i + 1` lags; one per possible lag up
    /// to the estimate spacing.
    pub(super) const HYPOTHESES: usize = Self::ACF_STEP;
    /// Widest comb element reaches 3 lags below its harmonic, and the top
    /// hypothesis is where its widest element still reads inside the window.
    pub(super) const PERIOD_INDEX: std::ops::RangeInclusive<usize> = (Self::COMB_HARMONICS - 1)
        ..=((Self::ACF_FRAME - (Self::COMB_HARMONICS - 1)) / Self::COMB_HARMONICS - 1);
    /// Adaptive-threshold half window, 0.1 s of detection frames.
    pub(super) const SMOOTH_HALF: usize = 8;
    /// Between-estimate spread of the period, in lags.
    pub(super) const TRANSITION_SIGMA: f32 = 8.0;
    /// The Gaussian transition's support, in lags.
    pub(super) const TRANSITION_SUPPORT: f32 = 32.0;
}

pub(super) struct DecodeConsts;

impl DecodeConsts {
    /// Height scale of the interval density: the Gaussian claims about 0.43
    /// of each state's transition mass, keeping every beat transition soft.
    pub(super) const DENSITY_SCALE: f32 = 0.005;
    pub(super) const EPSILON: f32 = 1e-6;
    /// Observations top out below one, so a skipped peak stays payable.
    pub(super) const OBSERVED_CEILING: f32 = 0.99;
    /// How far past the longest period the state space reaches, in standard
    /// deviations: the longest wait the decoder can express.
    pub(super) const STATE_MARGIN: f32 = 3.0;
    /// The interval density's support, in standard deviations.
    pub(super) const SUPPORT: f32 = 4.0;
}

pub(super) struct TrackerConsts;

impl TrackerConsts {
    /// A mark is never a certainty, and never nothing.
    pub(super) const CONFIDENCE_BOUNDS: (f32, f32) = (0.001, 0.999);
}
