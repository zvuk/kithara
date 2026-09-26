use num_traits::cast::ToPrimitive;

use super::consts;

pub(crate) fn frame_seconds() -> f32 {
    consts::FRAMES_HOP.to_f32().unwrap_or(1.0) / consts::FRAMES_RATE
}

pub(crate) fn seconds(frame: f32) -> f32 {
    frame * consts::FRAMES_HOP.to_f32().unwrap_or(1.0) / consts::FRAMES_RATE
}
