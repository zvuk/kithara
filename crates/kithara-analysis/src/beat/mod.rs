mod analyzer;
#[cfg(feature = "beat-backend")]
mod backend;
mod detector;
mod grid;
mod runs;

pub(crate) use analyzer::{BeatAnalyzer, BeatPassConfig, DetectOutput, DetectRequest};
#[cfg(feature = "beat-backend")]
pub(crate) use backend::{BeatDetectorKind, SELECTED_DETECTOR, build_detector};
#[cfg(feature = "beat-backend")]
pub(crate) use grid::GRID_SEMANTICS_TAG;
pub(crate) use grid::{GridParams, extend_over};
pub(crate) use kithara_beat::BeatDetector;
#[cfg(test)]
pub(crate) use kithara_beat::{BeatDetectorMock, BeatMark, RawBeats};
#[cfg(feature = "beat-backend")]
pub(crate) use runs::consts::DETECTOR_AUDIO_TAG;
