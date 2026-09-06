#[cfg(not(feature = "beat-backend"))]
mod disabled;
#[cfg(feature = "beat-backend")]
mod enabled;

#[cfg(all(not(feature = "beat-backend"), feature = "analysis-beat"))]
pub(crate) use disabled::*;
#[cfg(feature = "beat-backend")]
pub(crate) use enabled::*;
