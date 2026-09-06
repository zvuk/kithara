mod analyzer;
#[cfg(feature = "beat-backend")]
mod backend;
mod detector;
#[path = "../grid/mod.rs"]
mod grid;
mod pass;
mod runs;

pub(crate) use analyzer::{BeatPassConfig, DetectOutput, DetectRequest};
#[cfg(feature = "beat-backend")]
pub(crate) use backend::{BeatDetectorKind, SELECTED_DETECTOR, build_detector};
pub(crate) use detector::BeatDetector;
#[cfg(test)]
pub(crate) use detector::{BeatDetectError, BeatDetectorMock, BeatMark, RawBeats};
#[cfg(feature = "beat-backend")]
pub(crate) use grid::GRID_SEMANTICS_TAG;
pub(crate) use grid::GridParams;
pub(crate) use pass::BeatPass;
#[cfg(feature = "beat-backend")]
pub(crate) use runs::DETECTOR_AUDIO_TAG;
