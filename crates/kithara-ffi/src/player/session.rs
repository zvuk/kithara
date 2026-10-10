use super::AudioPlayer;
use crate::types::FfiInterruptionKind;

/// Platform audio-session signals the player answers: interruptions.
#[cfg_attr(all(feature = "uniffi", not(target_arch = "wasm32")), uniffi::export)]
impl AudioPlayer {
    /// Notify the native player that the platform interrupted, or released,
    /// the audio output.
    ///
    /// An interruption stops the output below the engine: the audio callback
    /// is no longer invoked, so playback can neither observe the interruption
    /// nor report it, and every value the audio thread publishes freezes where
    /// it stood. Reporting it here is what keeps the observable playback state
    /// honest while nothing is audible. Getting the output back is a route
    /// change, `notify_audio_route_changed`.
    pub fn notify_interruption(&self, kind: FfiInterruptionKind) {
        self.inner.notify_interruption(kind.into());
    }
}
