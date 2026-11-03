mod core;
mod interval;
mod plan;
mod source;

pub use core::{
    PhaseError, Slew, SlewCause, checked_correction, covers, entry, jump_target, phase_error, slew,
    speed, speed_curve,
};

pub use plan::{CorrectionPlan, CorrectionStep};
pub use source::{SourceGrid, SourceGridError};

#[cfg(test)]
mod tests;
