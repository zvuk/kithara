use bon::Builder;

use crate::Band;

struct Consts;

impl Consts {
    const BAND_GAIN: [f32; Band::COUNT] = [1.0, 2.5, 12.0];
    const ENERGY_FLOOR: f32 = 1e-4;
    const FFT_SIZE: usize = 4096;
    const LOW_MID_HZ: f32 = 250.0;
    const MID_HIGH_HZ: f32 = 2500.0;
}

/// FFT / band-split / reduction tunables. One home for the constants.
#[kithara_config::config(builder = false)]
#[derive(Builder, Clone, Copy, Debug, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
#[fieldwork(get)]
#[derive(kithara_derive::BuiltDefault)]
pub struct AnalysisParams {
    /// Per-band perceptual gain (`[low, mid, high]`) applied to magnitudes
    /// before shared normalization. Music tilts energy toward the low end, so
    /// without lifting mid/high the upper bands render as invisible slivers.
    /// This is the balance knob, not a color: low stays the dominant hull.
    #[config(value, builder(default = Consts::BAND_GAIN), field(get, copy))]
    band_gain: [f32; Band::COUNT],
    /// Per-window RMS gate; windows below it contribute no band energy.
    #[config(value, builder(default = Consts::ENERGY_FLOOR))]
    energy_floor: f32,
    /// Low/mid crossover in Hz.
    #[config(value, builder(default = Consts::LOW_MID_HZ))]
    low_mid_hz: f32,
    /// Mid/high crossover in Hz.
    #[config(value, builder(default = Consts::MID_HIGH_HZ))]
    mid_high_hz: f32,
    /// FFT window length (real input); band bins span `0..=fft_size/2`.
    #[config(value, builder(default = Consts::FFT_SIZE))]
    fft_size: usize,
}
