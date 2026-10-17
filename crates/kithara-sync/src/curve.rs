use std::num::NonZeroU64;

use kithara_platform::sync::Arc;
use kithara_signal::ExponentialApproach;

/// Speed a renderer holds from the output frame a command applies on.
#[derive(Clone, Debug, PartialEq)]
pub enum SpeedCurve {
    /// One speed: media seconds consumed per output second.
    Constant(f32),
    /// Analytic speed approach with a time constant in physical output frames.
    Approach(ExponentialApproach),
    /// Ordered analytic segments, each relative to its physical output-frame boundary.
    ApproachSteps(Arc<[(u64, ExponentialApproach)]>),
    /// Linear speed change from the speed at acceptance, then holds `to`.
    Ramp { to: f32, frames: NonZeroU64 },
    /// Speeds at strictly increasing output-frame offsets, then holds the last speed.
    Steps(Arc<[(u64, f32)]>),
}
