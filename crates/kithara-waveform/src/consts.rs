pub(crate) const BAND_GAIN: [f32; 4] = [1.0; 4];
pub(crate) const BAND_EDGES_HZ: [f64; 3] =
    [155.2404142063467, 1039.2304845413262, 6956.951290818225];
pub(crate) const COLUMNS: usize = 48;
#[cfg(feature = "dsp")]
pub(crate) const WINDOW_COLUMNS: usize = 64;
#[cfg(feature = "dsp")]
pub(crate) const MAX_PARTIAL: usize = 256;

#[cfg(all(test, feature = "dsp", not(target_arch = "wasm32")))]
pub(crate) const SR: u32 = 44_100;
