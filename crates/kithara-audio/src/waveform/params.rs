use bon::Builder;

struct Consts;

impl Consts {
    const BAND_GAIN: [f32; 3] = [1.0, 2.5, 12.0];
    const ENERGY_FLOOR: f32 = 1e-4;
    const FFT_SIZE: usize = 4096;
    const LOW_MID_HZ: f32 = 250.0;
    const MID_HIGH_HZ: f32 = 2500.0;
}

/// FFT / band-split / reduction tunables. One home for the constants.
#[derive(Builder, Clone, Copy, Debug)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AnalysisParams {
    /// Per-band perceptual gain (`[low, mid, high]`) applied to magnitudes
    /// before shared normalization. Music tilts energy toward the low end, so
    /// without lifting mid/high the upper bands render as invisible slivers.
    /// This is the balance knob, not a color: low stays the dominant hull.
    #[builder(default = Consts::BAND_GAIN)]
    band_gain: [f32; 3],
    /// Per-window RMS gate; windows below it contribute no band energy.
    #[builder(default = Consts::ENERGY_FLOOR)]
    energy_floor: f32,
    /// Low/mid crossover in Hz.
    #[builder(default = Consts::LOW_MID_HZ)]
    low_mid_hz: f32,
    /// Mid/high crossover in Hz.
    #[builder(default = Consts::MID_HIGH_HZ)]
    mid_high_hz: f32,
    /// FFT window length (real input); band bins span `0..=fft_size/2`.
    #[builder(default = Consts::FFT_SIZE)]
    fft_size: usize,
}

impl AnalysisParams {
    #[must_use]
    pub fn band_gain(&self) -> [f32; 3] {
        self.band_gain
    }

    #[must_use]
    pub fn energy_floor(&self) -> f32 {
        self.energy_floor
    }

    #[must_use]
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    #[must_use]
    pub fn low_mid_hz(&self) -> f32 {
        self.low_mid_hz
    }

    #[must_use]
    pub fn mid_high_hz(&self) -> f32 {
        self.mid_high_hz
    }
}

impl Default for AnalysisParams {
    fn default() -> Self {
        Self {
            fft_size: Consts::FFT_SIZE,
            low_mid_hz: Consts::LOW_MID_HZ,
            mid_high_hz: Consts::MID_HIGH_HZ,
            energy_floor: Consts::ENERGY_FLOOR,
            band_gain: Consts::BAND_GAIN,
        }
    }
}
