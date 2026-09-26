#[cfg(any(target_os = "macos", target_os = "ios"))]
#[cfg(all(test, feature = "resample-rubato"))]
pub(crate) const CHUNK_FRAMES: usize = 1024;

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[cfg(all(test, feature = "resample-rubato"))]
pub(crate) const DRAIN_LIMIT: usize = 16;

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[cfg(all(test, feature = "resample-rubato"))]
pub(crate) const FLUSH_FRAME_TOLERANCE: usize = 1;

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[cfg(all(test, feature = "resample-rubato"))]
pub(crate) const PASSTHROUGH_RMS_TOLERANCE: f64 = 0.000_01;

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[cfg(all(test, feature = "resample-rubato"))]
pub(crate) const SHAPE_ENERGY_DELTA_TOLERANCE: f64 = 0.12;
