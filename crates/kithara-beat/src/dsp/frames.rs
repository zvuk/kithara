//! The time grid every stage of the detector is expressed on.

use num_traits::cast::ToPrimitive;

/// The rate the crate contract fixes.
pub(crate) const RATE: f32 = 22_050.0;
/// Analysis window, 46.4 ms.
pub(crate) const FRAME: usize = 1024;
/// Hop, 11.61 ms: the detection-function resolution the papers fix. The
/// reference reaches it by computing at twice that and interpolating;
/// computing it directly tracked the reference's own output more closely.
pub(crate) const HOP: usize = 256;
/// Deviation the time between consecutive beats is allowed, the paper's
/// 0.02 s. The period tracker and the beat decoder both read it.
const SIGMA_SECONDS: f32 = 0.02;

/// Seconds per frame, the same 11.61 ms.
pub(crate) fn frame_seconds() -> f32 {
    HOP.to_f32().unwrap_or(1.0) / RATE
}

/// [`SIGMA_SECONDS`] in frames.
pub(crate) fn sigma() -> f32 {
    SIGMA_SECONDS / frame_seconds()
}

/// A frame's time. Frames carry the time of their window centre: a spectral
/// difference peaks when the transient reaches the middle of the window.
pub(crate) fn seconds(frame: f32) -> f32 {
    let offset = (FRAME / 2).to_f32().unwrap_or(0.0);
    (frame * HOP.to_f32().unwrap_or(1.0) + offset) / RATE
}
