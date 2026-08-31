//! The time grid every stage of the detector is expressed on.

use num_traits::cast::ToPrimitive;

/// The rate the crate contract fixes.
pub(crate) const RATE: f32 = 22_050.0;
/// Analysis window, 46.4 ms.
pub(crate) const FRAME: usize = 1024;
/// Hop, 11.61 ms: the detection-function resolution the papers fix.
pub(crate) const HOP: usize = 256;
/// Deviation allowed between consecutive beats.
pub(crate) const SIGMA_SECONDS: f32 = 0.025;

pub(crate) fn frame_seconds() -> f32 {
    HOP.to_f32().unwrap_or(1.0) / RATE
}

pub(crate) fn sigma() -> f32 {
    SIGMA_SECONDS / frame_seconds()
}

/// A frame's time, taken at its window start.
pub(crate) fn seconds(frame: f32) -> f32 {
    frame * HOP.to_f32().unwrap_or(1.0) / RATE
}
