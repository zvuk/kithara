//! Pure synchronization mathematics over track grids and session tempo steps.
#![forbid(unsafe_code)]

mod anchor;
mod consts;
mod curve;
mod error;
mod grid;
mod math;
mod meter;
mod tempo;
mod trajectory;

pub use anchor::{CoordinateError, SessionAnchor, SessionBeat};
pub use consts::MIN_SPEED;
pub use curve::SpeedCurve;
pub use error::TrajectoryError;
pub use grid::{TrackBeat, TrackGrid, TrackGridError, boundary};
pub use kithara_signal::Bound;
pub use math::{
    CorrectionPlan, CorrectionStep, PhaseError, SourceGrid, SourceGridError, checked_correction,
    corrected_speed_curve, covers, entry, jump_target, phase_error, speed, speed_curve,
};
pub use meter::Meter;
pub use tempo::{Tempo, TempoError};
pub use trajectory::{TempoStep, TempoTrajectory};
