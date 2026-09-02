use delegate::delegate;
use kithara_abr::AbrHandle;
use kithara_audio::SeekOutcome;
use kithara_bufpool::HasPool;
use kithara_events::{EventBus, TrackId};
use kithara_platform::sync::Arc;

use super::{PlayerRuntime, SelectTransition};
use crate::{
    EngineLoadSnapshot, EqBandConfig, PlayError, PlaybackSnapshot, PlayerStatus, Resource,
    ResourceConfig,
};

/// Cloneable runtime capability used by player-owned orchestration.
///
/// The handle deliberately excludes beat-grid identity, synchronization
/// topology, and engine/session getters. Closing the resident player
/// invalidates every outstanding clone through the shared runtime gate.
pub struct PlayerControl<S> {
    runtime: Arc<PlayerRuntime<S>>,
}

impl<S> Clone for PlayerControl<S> {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
        }
    }
}

impl<S> PlayerControl<S>
where
    S: HasPool<f32>,
{
    pub(super) fn new(runtime: Arc<PlayerRuntime<S>>) -> Self {
        Self { runtime }
    }

    fn command(&self, command: impl FnOnce(&PlayerRuntime<S>)) {
        let _ = self.runtime.with_open(command);
    }

    /// Root event bus used to scope per-track loader events.
    #[must_use]
    pub fn bus(&self) -> EventBus {
        self.runtime.bus().clone()
    }

    /// Prepare one resource for this player's runtime.
    pub fn prepare_config<B>(
        &self,
        config: ResourceConfig<S, B>,
    ) -> Result<ResourceConfig<S, B>, PlayError>
    where
        B: Clone + Default,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        self.runtime
            .with_open_result(|runtime| runtime.prepare_config(config))
    }

    /// Plant a completed resource into an existing player slot.
    pub fn replace_item(
        &self,
        index: usize,
        resource: Resource,
        item_id: TrackId,
    ) -> Result<(), PlayError> {
        self.runtime
            .with_open(|runtime| runtime.replace_item(index, resource, item_id))
    }

    /// Remove every queued player resource.
    pub fn remove_all_items(&self) {
        self.command(PlayerRuntime::remove_all_items);
    }

    /// Remove one queued resource.
    pub fn remove_at(&self, index: usize) -> Result<Option<Resource>, PlayError> {
        self.runtime.with_open(|runtime| runtime.remove_at(index))
    }

    /// Reserve queue slots in the resident player.
    pub fn reserve_slots(&self, count: usize) {
        self.command(|runtime| runtime.reserve_slots(count));
    }

    /// Discard one prepared player item.
    pub fn clear_item(&self, index: usize) {
        self.command(|runtime| runtime.clear_item(index));
    }

    /// Start or resume playback unless the owning player is closed.
    pub fn play(&self) {
        self.command(PlayerRuntime::play);
    }

    /// Pause playback unless the owning player is closed.
    pub fn pause(&self) {
        self.command(PlayerRuntime::pause);
    }

    /// Seek within the current player item.
    pub fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        self.runtime
            .with_open_result(|runtime| runtime.seek_seconds(seconds))
    }

    /// Advance player control-plane work.
    pub fn tick(&self) -> Result<(), PlayError> {
        self.runtime.with_open_result(PlayerRuntime::tick)
    }

    /// Restart the current output route.
    pub fn invalidate_audio_route(&self, reason: &str) -> Result<(), PlayError> {
        self.runtime
            .with_open_result(|runtime| runtime.invalidate_audio_route(reason))
    }

    /// Drain pending player notifications.
    pub fn process_notifications(&self) {
        self.command(PlayerRuntime::process_notifications);
    }

    /// Whether the resident player is currently active.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        !self.runtime.is_closed() && self.runtime.is_playing()
    }

    /// Whether playback is explicitly paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.runtime.is_closed() || self.runtime.is_paused()
    }

    delegate! {
        to self.runtime {
            /// Whether the owning player has been closed or is closing.
            #[must_use]
            pub fn is_closed(&self) -> bool;
            /// Close the player runtime and invalidate every outstanding control.
            ///
            /// # Errors
            ///
            /// Returns the session detach failure and reopens the command gate so the
            /// owner can retry without losing lifecycle ownership.
            pub fn close(&self) -> Result<(), PlayError>;
            /// Configured crossfade duration in seconds.
            #[must_use]
            pub fn crossfade_duration(&self) -> f32;
            /// Current queue item index in the resident player.
            #[must_use]
            pub fn current_index(&self) -> usize;
            /// Whether one player item still owns a prepared resource.
            #[must_use]
            pub fn item_has_resource(&self, index: usize) -> bool;
            /// Latest playback position.
            #[must_use]
            pub fn position_seconds(&self) -> Option<f64>;
            /// Latest coherent playback state.
            #[must_use]
            pub fn playback_snapshot(&self) -> Option<PlaybackSnapshot>;
            /// Current ABR handle for the active item.
            #[must_use]
            pub fn current_abr_handle(&self) -> Option<AbrHandle>;
            /// Current live playback rate.
            #[must_use]
            pub fn rate(&self) -> f32;
            /// Rate the player's master bus runs at.
            #[must_use]
            pub fn sample_rate(&self) -> u32;
            /// Configured default playback rate.
            #[must_use]
            pub fn default_rate(&self) -> f32;
            /// Current output volume.
            #[must_use]
            pub fn volume(&self) -> f32;
            /// Whether output is muted.
            #[must_use]
            pub fn is_muted(&self) -> bool;
            /// Current player status.
            #[must_use]
            pub fn status(&self) -> PlayerStatus;
            /// Current engine cost snapshot.
            #[must_use]
            pub fn engine_load(&self) -> EngineLoadSnapshot;
            /// Number of EQ bands.
            #[must_use]
            pub fn eq_band_count(&self) -> usize;
            /// Gain of one EQ band.
            #[must_use]
            pub fn eq_gain(&self, band: usize) -> Option<f32>;
            /// Current item duration.
            #[must_use]
            pub fn duration_seconds(&self) -> Option<f64>;
        }
    }

    /// Update crossfade duration unless the owning player is closed.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        self.command(|runtime| runtime.set_crossfade_duration(seconds));
    }

    /// Update the default playback rate unless the owning player is closed.
    pub fn set_default_rate(&self, rate: f32) {
        self.command(|runtime| runtime.set_default_rate(rate));
    }

    /// Update live playback rate unless the owning player is closed.
    pub fn set_rate(&self, rate: f32) {
        self.command(|runtime| runtime.set_rate(rate));
    }

    /// Update output volume unless the owning player is closed.
    pub fn set_volume(&self, volume: f32) {
        self.command(|runtime| runtime.set_volume(volume));
    }

    /// Update mute state unless the owning player is closed.
    pub fn set_muted(&self, muted: bool) {
        self.command(|runtime| runtime.set_muted(muted));
    }

    /// Update one EQ band.
    pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        self.runtime
            .with_open_result(|runtime| runtime.set_eq_gain(band, gain_db))
    }

    /// Replace the EQ band layout.
    pub fn set_eq_layout(&self, layout: Vec<EqBandConfig>) -> Result<(), PlayError> {
        self.runtime
            .with_open_result(|runtime| runtime.set_eq_layout(layout))
    }

    /// Reset all EQ bands.
    pub fn reset_eq(&self) -> Result<(), PlayError> {
        self.runtime.with_open_result(PlayerRuntime::reset_eq)
    }

    /// Apply a completed selection through the resident player runtime.
    pub fn select_item_with_crossfade(
        &self,
        index: usize,
        transition: SelectTransition,
    ) -> Result<(), PlayError> {
        self.runtime
            .with_open_result(|runtime| runtime.select_item_with_crossfade(index, transition))
    }
}
