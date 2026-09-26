use crate::band::Band;

#[cfg(feature = "dsp")]
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const EPS: f32 = 1e-6;

#[cfg(feature = "dsp")]
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const SR: u32 = 44_100;

pub(crate) const BAND_GAIN: [f32; Band::COUNT] = [1.0, 2.5, 12.0];
pub(crate) const ENERGY_FLOOR: f32 = 1e-4;
pub(crate) const FFT_SIZE: usize = 4096;
pub(crate) const LOW_MID_HZ: f32 = 250.0;
pub(crate) const MID_HIGH_HZ: f32 = 2500.0;
