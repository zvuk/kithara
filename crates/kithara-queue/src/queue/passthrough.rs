use delegate::delegate;
use kithara_events::EventBus;
use kithara_play::{EngineLoadSnapshot, PlayError, PlayerStatus};

use super::Queue;

impl Queue {
    pub fn set_crossfade_duration(&self, seconds: f32) {
        self.player.set_crossfade_duration(seconds);
        self.bus
            .publish(kithara_events::QueueEvent::CrossfadeDurationChanged {
                seconds: self.player.crossfade_duration(),
            });
    }

    delegate! {
        to self.player {
            /// Whether playback is active.
            #[must_use]
            pub fn is_playing(&self) -> bool;
            /// Current crossfade duration in seconds.
            #[must_use]
            pub fn crossfade_duration(&self) -> f32;
            /// Live engine playback rate (player-reported, 0.0 when paused).
            #[must_use]
            pub fn rate(&self) -> f32;
            /// Default playback rate.
            #[must_use]
            pub fn default_rate(&self) -> f32;
            /// Set the default playback rate.
            pub fn set_default_rate(&self, rate: f32);
            /// Set the live playback rate (mirrors into the tempo-mode sibling
            /// so a running key-locked stretch tracks the move).
            pub fn set_rate(&self, rate: f32);
            /// Current volume (0.0..=1.0).
            #[must_use]
            pub fn volume(&self) -> f32;
            /// Set the volume (0.0..=1.0).
            pub fn set_volume(&self, volume: f32);
            /// Whether output is muted.
            #[must_use]
            pub fn is_muted(&self) -> bool;
            /// Set the mute flag.
            pub fn set_muted(&self, muted: bool);
            /// Live engine playback status.
            #[must_use]
            pub fn status(&self) -> PlayerStatus;
            /// Live audio-engine cost (realtime factor / load / ms).
            #[must_use]
            pub fn engine_load(&self) -> EngineLoadSnapshot;
            /// Number of EQ bands.
            #[must_use]
            pub fn eq_band_count(&self) -> usize;
            /// Current gain for an EQ band.
            #[must_use]
            pub fn eq_gain(&self, band: usize) -> Option<f32>;
            /// Set gain for an EQ band.
            ///
            /// # Errors
            /// Forwards `PlayError` from the underlying player.
            pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError>;
            /// Reset all EQ bands to 0 dB.
            ///
            /// # Errors
            /// Forwards `PlayError` from the underlying player.
            pub fn reset_eq(&self) -> Result<(), PlayError>;
            /// Current track duration in seconds.
            #[must_use]
            pub fn duration_seconds(&self) -> Option<f64>;
            /// Underlying [`EventBus`]. FFI/TUI bridges use `.scoped()` /
            /// `.clone()` to wire their own subscriptions; typical callers
            /// should prefer [`Self::subscribe`].
            #[must_use]
            pub fn bus(&self) -> &EventBus;
            /// Drain pending player-side notifications. Called by FFI tick
            /// loops after [`Self::tick`].
            pub fn process_notifications(&self);
        }
    }
}
