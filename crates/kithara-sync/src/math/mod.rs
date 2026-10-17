mod core;
mod interval;
mod plan;
mod source;

pub use core::{
    PhaseError, checked_correction, corrected_speed_curve, covers, entry, jump_target, phase_error,
    speed, speed_curve,
};

pub use plan::{CorrectionPlan, CorrectionStep};
pub use source::{SourceGrid, SourceGridError};

#[cfg(test)]
mod tests;
