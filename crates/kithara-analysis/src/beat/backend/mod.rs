mod build;
#[cfg(feature = "beat-dsp")]
mod dsp;
mod kind;
#[cfg(feature = "beat-nn")]
mod nn;

pub(crate) use build::build_detector;
pub(crate) use kind::{BeatDetectorKind, SELECTED_DETECTOR};
