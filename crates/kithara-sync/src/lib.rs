//! Pure synchronization mathematics over track grids and session tempo steps.
#![forbid(unsafe_code)]

mod bound;
mod error;
mod math;
mod tempo;
mod trajectory;

pub use bound::Bound;
pub use error::TrajectoryError;
pub use math::{
    CorrectionPlan, CorrectionStep, PhaseError, checked_correction, correction, covers, entry,
    jump_target, phase_error, speed,
};
pub use tempo::{Tempo, TempoError};
pub use trajectory::{TempoStep, TempoTrajectory};
