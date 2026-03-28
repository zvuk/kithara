#![forbid(unsafe_code)]

/// Application lifecycle events.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AppEvent {
    /// Informational note (e.g. "track loaded", "ctrl+c received").
    Note(String),
    /// Graceful shutdown signal.
    Stop,
}
