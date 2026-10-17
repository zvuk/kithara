use std::num::NonZeroU128;

use kithara_signal::SourceSpan;
use kithara_sync::SpeedCurve;

#[derive(Clone, Copy)]
pub(in crate::render) struct Fraction {
    pub(in crate::render) numerator: u128,
    pub(in crate::render) denominator: NonZeroU128,
}

#[derive(Clone)]
/// Canonical source-phase execution and projection for an accepted speed law.
pub struct SourceTrajectory {
    pub(super) curve: SpeedCurve,
    pub(super) origin_speed: Fraction,
    pub(super) elapsed: u64,
    pub(super) position: Option<Phase>,
    pub(super) curve_origin: Option<(Fraction, u64)>,
}

/// Replacement origins use whole source frames and Q32 fractions, rounded to
/// nearest with exact half units toward the next frame. A running curve keeps
/// its exact integral until the next replacement.
#[derive(Clone, Copy)]
pub(super) enum Phase {
    Origin { frames: u64, fraction: u32 },
    Rendered(SourceSpan),
    Exact(Fraction),
}
