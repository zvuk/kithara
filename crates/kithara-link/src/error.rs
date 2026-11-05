/// A refused tempo step or synchronization configuration.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum LinkError {
    /// The tempo trajectory refused the change.
    #[error(transparent)]
    Trajectory(#[from] kithara_sync::TrajectoryError),
    /// Correction steps require a finite, positive speed limit.
    #[error("correction epsilon must be finite and positive, got {epsilon}")]
    Epsilon { epsilon: f32 },
    /// The phase a slew may close must be a finite, positive number of beats.
    #[error("slew limit must be finite and positive, got {limit}")]
    SlewLimit { limit: f32 },
    /// A speed plan must reach some time past its correction.
    #[error("speed plan horizon must be positive, got {horizon:?}")]
    Horizon {
        horizon: kithara_platform::time::Duration,
    },
}
