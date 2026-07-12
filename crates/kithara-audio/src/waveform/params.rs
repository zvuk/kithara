/// FFT / band-split / reduction tunables. One home for the constants.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct AnalysisParams {
    /// Per-band perceptual gain (`[low, mid, high]`) applied to magnitudes
    /// before shared normalization. Music tilts energy toward the low end, so
    /// without lifting mid/high the upper bands render as invisible slivers.
    /// This is the balance knob, not a color: low stays the dominant hull.
    pub band_gain: [f32; 3],
    /// Per-window RMS gate; windows below it contribute no band energy.
    pub energy_floor: f32,
    /// Low/mid crossover in Hz.
    pub low_mid_hz: f32,
    /// Mid/high crossover in Hz.
    pub mid_high_hz: f32,
    /// FFT window length (real input); band bins span `0..=fft_size/2`.
    pub fft_size: usize,
}

impl Default for AnalysisParams {
    fn default() -> Self {
        Self {
            fft_size: 4096,
            low_mid_hz: 250.0,
            mid_high_hz: 2500.0,
            energy_floor: 1e-4,
            band_gain: [1.0, 2.5, 12.0],
        }
    }
}
