#[cfg(feature = "stretch-bungee")]
#[cfg(test)]
pub(crate) const CHANNELS: usize = 2;

#[cfg(feature = "stretch-bungee")]
#[cfg(test)]
pub(crate) const CONTEXT_FRAMES: usize = 8192;

#[cfg(feature = "stretch-bungee")]
#[cfg(test)]
pub(crate) const SAMPLE_RATE: u32 = 48_000;

pub(crate) const CONTINUITY_TOLERANCE: f64 = 1.0e-6;
pub(crate) const MAX_CORRECTION_PER_BLOCK: f64 = 1.0;
pub(crate) const MAX_PHASE_ERROR: f64 = 1.0;
pub(crate) const MAX_SOURCE_FRAMES_PER_OUTPUT: f64 = 4.0;
pub(crate) const MIN_SOURCE_FRAMES_PER_OUTPUT: f64 = 0.05;
pub(crate) const MAX_SPANS: usize = 4;
