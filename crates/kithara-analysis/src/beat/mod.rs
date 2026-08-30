mod analyzer;
mod detector;
#[path = "../grid/mod.rs"]
mod grid;
mod pass;
mod runs;

pub(crate) use analyzer::{BeatPassConfig, DetectOutput, DetectRequest};
pub(crate) use detector::BeatDetector;
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) use detector::backend::{BeatDetectorKind, SELECTED_DETECTOR, build_detector};
#[cfg(test)]
pub(crate) use detector::{BeatDetectError, BeatDetectorMock, BeatMark, RawBeats};
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) use grid::GRID_SEMANTICS_TAG;
pub(crate) use grid::GridParams;
pub(crate) use pass::BeatPass;
