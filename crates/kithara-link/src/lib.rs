//! Track and Host synchronization on one owner-thread tempo trajectory.
#![forbid(unsafe_code)]

mod deck;
mod error;
mod factory;
mod grid;
mod host;
mod linked;
mod queue;

#[cfg(test)]
mod tests;

pub use deck::LinkedDeck;
pub use error::LinkError;
pub use factory::LinkedFactory;
pub use grid::{GridAnswer, GridRefusal};
pub use host::{LinkedHost, LinkedHostCommand, PendingTempo};
pub use kithara_sync::{
    CorrectionPlan, CorrectionStep, PhaseError, TempoStep, TempoTrajectory, correction, covers,
    entry, jump_target, phase_error, speed,
};
pub use linked::{LinkConfig, Linked, LinkedPlayer, LinkedSnapshot, SyncMode, SyncStatus, Waiting};
