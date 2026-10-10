mod core;
mod plan;

pub use core::{
    PhaseError, checked_correction, correction, covers, entry, jump_target, phase_error, speed,
};

pub use plan::{CorrectionPlan, CorrectionStep};

#[cfg(test)]
mod tests;
