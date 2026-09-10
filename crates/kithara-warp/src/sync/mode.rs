/// How a synchronization group derives its tempo and phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncMode {
    /// Follows the parent group's tempo and phase.
    HostSync,
    /// Owns its tempo; its members follow it.
    LocalSync,
    /// No beat timeline: playback rate is a plain multiplier.
    Off,
}
