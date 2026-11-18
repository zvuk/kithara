/// Failure to prepare or transfer a complete offline mixer turn.
#[derive(Debug, thiserror::Error)]
pub enum OfflineDeckError {
    #[error("offline mixer is not owned by the session")]
    MissingMixer,
    #[error("offline mixer turn is still in flight")]
    TurnInFlight,
    #[error("offline turn must fit its nonempty prepared PCM range")]
    InvalidFrames,
}
