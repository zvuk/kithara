use std::num::NonZeroU64;

use kithara_platform::sync::Arc;

/// Speed a renderer holds from the output frame a command applies on.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SpeedCurve {
    /// One speed: media seconds consumed per output second.
    Constant(f32),
    /// Linear speed change from the speed at acceptance, then holds `to`.
    Ramp { to: f32, frames: NonZeroU64 },
    /// Speeds at strictly increasing output-frame offsets, then holds the last speed.
    Steps(Arc<[(u64, f32)]>),
}
