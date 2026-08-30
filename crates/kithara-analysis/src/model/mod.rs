#[cfg(not(any(feature = "beat-nn", feature = "beat-dsp")))]
mod disabled;
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
mod enabled;

#[cfg(not(any(feature = "beat-nn", feature = "beat-dsp")))]
pub(crate) use disabled::*;
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) use enabled::*;
