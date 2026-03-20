use std::{sync::Arc, time::Duration};

use kithara::prelude::PlayerImpl;
use tracing::error;

use crate::playlist::Playlist;

/// Player control contract for UI frontends.
///
/// Frontends call these methods in response to user input.
/// They do not interact with `PlayerImpl` or engine internals directly.
pub trait PlayerControls {
    // -- Playback --

    fn play(&mut self);
    fn pause(&mut self);
    fn toggle_play_pause(&mut self);
    fn seek(&mut self, position: Duration);
    fn set_volume(&mut self, volume: f32);

    // -- Playlist --

    /// Select a track by index and start playback.
    ///
    /// # Errors
    /// Returns an error if the track cannot be loaded.
    fn select_track(&mut self, index: usize) -> Result<(), AppControllerError>;
    fn next_track(&mut self) -> Option<usize>;
    fn prev_track(&mut self) -> Option<usize>;

    // -- EQ --

    fn set_eq_band(&mut self, band: usize, gain_db: f32);
    fn reset_eq_band(&mut self, band: usize);

    // -- Crossfade --

    fn set_crossfade_duration(&mut self, seconds: f32);

    // -- Toggles --

    fn toggle_shuffle(&mut self) -> bool;
    fn toggle_repeat(&mut self) -> bool;

    // -- Read-only state --

    fn is_playing(&self) -> bool;
    fn position(&self) -> Option<Duration>;
    fn duration(&self) -> Option<Duration>;
    fn volume(&self) -> f32;
    fn current_track_index(&self) -> Option<usize>;
    fn track_count(&self) -> usize;
    fn track_name(&self, index: usize) -> String;
    fn crossfade_duration(&self) -> f32;
    fn eq_bands(&self) -> &[f32];
    fn playlist(&self) -> &Playlist;

    /// Call the player's periodic tick. Should be called from the UI loop.
    fn tick(&mut self);
}

pub type AppControllerError = Box<dyn std::error::Error + Send + Sync>;

/// Concrete implementation of [`PlayerControls`] backed by [`PlayerImpl`].
pub struct AppController {
    player: Arc<PlayerImpl>,
    playlist: Arc<Playlist>,
    eq_bands_state: Vec<f32>,
    repeat_enabled: bool,
}

impl AppController {
    #[must_use]
    pub fn new(player: Arc<PlayerImpl>, playlist: Arc<Playlist>, eq_band_count: usize) -> Self {
        Self {
            player,
            playlist,
            eq_bands_state: vec![0.0; eq_band_count],
            repeat_enabled: false,
        }
    }

    /// Access the underlying player (for event subscriptions, etc.).
    #[must_use]
    pub fn player(&self) -> &Arc<PlayerImpl> {
        &self.player
    }
}

impl PlayerControls for AppController {
    fn play(&mut self) {
        self.player.play();
    }

    fn pause(&mut self) {
        self.player.pause();
    }

    fn toggle_play_pause(&mut self) {
        if self.player.is_playing() {
            self.player.pause();
        } else {
            self.player.play();
        }
    }

    fn seek(&mut self, position: Duration) {
        if let Err(e) = self.player.seek_seconds(position.as_secs_f64()) {
            error!("seek failed: {e:?}");
        }
    }

    fn set_volume(&mut self, volume: f32) {
        self.player.set_volume(volume.clamp(0.0, 1.0));
    }

    fn select_track(&mut self, index: usize) -> Result<(), AppControllerError> {
        if index >= self.playlist.len() {
            return Err(format!("track index {index} out of range").into());
        }
        self.playlist.on_track_selected(index);
        Ok(())
    }

    fn next_track(&mut self) -> Option<usize> {
        self.playlist.get_next_track()
    }

    fn prev_track(&mut self) -> Option<usize> {
        self.playlist.get_prev_track()
    }

    fn set_eq_band(&mut self, band: usize, gain_db: f32) {
        if band < self.eq_bands_state.len() {
            self.eq_bands_state[band] = gain_db;
            if let Err(e) = self.player.set_eq_gain(band, gain_db) {
                error!("set EQ gain band={band} db={gain_db:.1} failed: {e:?}");
            }
        }
    }

    fn reset_eq_band(&mut self, band: usize) {
        self.set_eq_band(band, 0.0);
    }

    fn set_crossfade_duration(&mut self, seconds: f32) {
        self.player.set_crossfade_duration(seconds);
    }

    fn toggle_shuffle(&mut self) -> bool {
        self.playlist.toggle_shuffle()
    }

    fn toggle_repeat(&mut self) -> bool {
        self.repeat_enabled = !self.repeat_enabled;
        self.repeat_enabled
    }

    fn is_playing(&self) -> bool {
        self.player.is_playing()
    }

    fn position(&self) -> Option<Duration> {
        self.player
            .position_seconds()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(Duration::from_secs_f64)
    }

    fn duration(&self) -> Option<Duration> {
        self.player
            .duration_seconds()
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(Duration::from_secs_f64)
    }

    fn volume(&self) -> f32 {
        self.player.volume()
    }

    fn current_track_index(&self) -> Option<usize> {
        Some(self.player.current_index())
    }

    fn track_count(&self) -> usize {
        self.playlist.len()
    }

    fn track_name(&self, index: usize) -> String {
        self.playlist.track_name(index)
    }

    fn crossfade_duration(&self) -> f32 {
        self.player.crossfade_duration()
    }

    fn eq_bands(&self) -> &[f32] {
        &self.eq_bands_state
    }

    fn playlist(&self) -> &Playlist {
        &self.playlist
    }

    fn tick(&mut self) {
        if let Err(e) = self.player.tick() {
            error!("tick failed: {e}");
        }
    }
}
