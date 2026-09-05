//! The time grid every stage of the detector is expressed on.

use num_traits::cast::ToPrimitive;

use super::consts::FramesConsts;

pub(crate) fn frame_seconds() -> f32 {
    FramesConsts::HOP.to_f32().unwrap_or(1.0) / FramesConsts::RATE
}

pub(crate) fn sigma() -> f32 {
    FramesConsts::SIGMA_SECONDS / frame_seconds()
}

/// A frame's time, taken at its window start.
pub(crate) fn seconds(frame: f32) -> f32 {
    frame * FramesConsts::HOP.to_f32().unwrap_or(1.0) / FramesConsts::RATE
}
