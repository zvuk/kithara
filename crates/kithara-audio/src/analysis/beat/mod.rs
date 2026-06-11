mod analyzer;
mod detector;
#[cfg(feature = "beat-nn")]
mod detector_factory;
#[cfg(feature = "beat-nn")]
mod detector_kind;
mod grid;

pub(crate) use analyzer::{BeatPass, SharedBeatDetector};
#[cfg(test)]
pub(crate) use detector::{BeatDetector, BeatDetectorMock, RawBeats};
pub(crate) use grid::GridParams;

#[cfg(feature = "beat-nn")]
pub(crate) use self::{detector_factory::build_detector, detector_kind::BeatDetectorKind};
