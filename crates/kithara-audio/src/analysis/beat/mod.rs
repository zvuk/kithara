mod analyzer;
mod detector;
#[path = "../grid/mod.rs"]
mod grid;
mod pass;

pub(crate) use analyzer::BeatPassConfig;
pub(crate) use detector::BeatDetector;
#[cfg(feature = "beat-nn")]
pub(crate) use detector::backend::{BeatDetectorKind, build_detector};
#[cfg(test)]
pub(crate) use detector::{BeatDetectorMock, RawBeats};
#[cfg(feature = "beat-nn")]
pub(crate) use grid::GRID_SEMANTICS_TAG;
pub(crate) use grid::{GridParams, GridPool};
pub(crate) use pass::BeatPass;
