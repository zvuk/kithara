//! `delegate!`-forwarded `PlayerImpl` controls (play/pause/volume/EQ/...).
//! Pure delegation; no queue-level logic.

use delegate::delegate;
use kithara_play::PlayError;

use super::Queue;

impl Queue {
    delegate! {
        to self.player {
            /// Start playback.
            pub fn play(&self);
            /// Pause playback.
            pub fn pause(&self);
            /// Whether playback is active.
            #[must_use]
            pub fn is_playing(&self) -> bool;
            /// Current crossfade duration in seconds.
            #[must_use]
            pub fn crossfade_duration(&self) -> f32;
            /// Set the crossfade duration.
            pub fn set_crossfade_duration(&self, seconds: f32);
            /// Default playback rate.
            #[must_use]
            pub fn default_rate(&self) -> f32;
            /// Set the default playback rate.
            pub fn set_default_rate(&self, rate: f32);
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
        }
    }
}
