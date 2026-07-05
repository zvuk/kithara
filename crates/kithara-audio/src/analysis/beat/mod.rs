mod analyzer;
mod consts;
mod detector;
#[cfg(feature = "beat-nn")]
mod detector_factory;
#[cfg(feature = "beat-nn")]
mod detector_kind;
mod grid;
mod resampler;
#[cfg(feature = "resample-fft")]
mod resampler_fft;
#[cfg(not(feature = "resample-fft"))]
mod resampler_sinc;

pub(crate) use analyzer::{BeatPass, SharedBeatDetector};
pub(in crate::analysis::beat) use consts::{BLOCK_FRAMES, TARGET_RATE};
#[cfg(test)]
pub(crate) use detector::{BeatDetector, BeatDetectorMock, RawBeats};
pub(crate) use grid::GridParams;

#[cfg(feature = "beat-nn")]
pub(crate) use self::{detector_factory::build_detector, detector_kind::BeatDetectorKind};
