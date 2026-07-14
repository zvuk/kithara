#[cfg(not(feature = "beat-nn"))]
mod disabled;
#[cfg(feature = "beat-nn")]
mod enabled;

#[cfg(not(feature = "beat-nn"))]
pub(crate) use disabled::*;
#[cfg(feature = "beat-nn")]
pub(crate) use enabled::*;
