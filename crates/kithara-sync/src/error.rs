use kithara_signal::SessionFrame;

/// A refused change to a tempo trajectory.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TrajectoryError {
    /// A new step must come after the trajectory's initial anchor.
    #[error("tempo step at {frame:?} is not after the initial anchor")]
    BeforeAnchor { frame: SessionFrame },
    /// Each frame names at most one tempo change.
    #[error("a tempo step already occupies {frame:?}")]
    Occupied { frame: SessionFrame },
}
