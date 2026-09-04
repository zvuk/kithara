mod analyzer;
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
mod backend;
mod detector;
#[path = "../grid/mod.rs"]
mod grid;
mod pass;
mod runs;

pub(crate) use analyzer::{BeatPassConfig, DetectOutput, DetectRequest};
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) use backend::{BeatDetectorKind, SELECTED_DETECTOR, build_detector};
pub(crate) use detector::BeatDetector;
#[cfg(test)]
pub(crate) use detector::{BeatDetectError, BeatDetectorMock, BeatMark, RawBeats};
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) use grid::GRID_SEMANTICS_TAG;
pub(crate) use grid::GridParams;
pub(crate) use pass::BeatPass;
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) use runs::DETECTOR_AUDIO_TAG;
pub(crate) use runs::Intake;
