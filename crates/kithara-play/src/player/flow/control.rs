use tracing::{debug, warn};

use super::super::core::PlayerImpl;
use crate::{
    api::{PlayerEvent, SlotId},
    bridge::PlayerCmd,
    error::PlayError,
};

impl PlayerImpl {
    /// Apply effective volume to the current slot (per-instance).
    pub(crate) fn apply_effective_volume(&self, volume: f32) {
        if let Some(slot_id) = self.slot() {
            debug!(volume, ?slot_id, "applying effective volume to slot");
            if let Err(e) = self.core.engine.set_slot_volume(slot_id, volume) {
                warn!(?e, volume, "failed to set slot volume");
            }
        } else {
            debug!(volume, "apply_effective_volume: no slot allocated yet");
        }
    }

    /// Ensure we have an active slot, allocating one if needed.
    pub fn ensure_slot(&self) -> Result<SlotId, PlayError> {
        if let Some(id) = self.slot() {
            return Ok(id);
        }
        let id = self.core.engine.allocate_slot()?;
        self.enter_loading_with_slot(id);
        let effective = if self.is_muted() { 0.0 } else { self.volume() };
        self.core.engine.set_slot_volume(id, effective)?;
        Ok(id)
    }

    /// Notify the audio host that the platform route changed and the
    /// native output stream must be recreated if playback is active.
    pub fn invalidate_audio_route(&self, reason: &str) -> Result<(), PlayError> {
        self.core.engine.invalidate_audio_route(reason)
    }

    /// Reset EQ gains to 0 dB for all bands.
    pub fn reset_eq(&self) -> Result<(), PlayError> {
        let slot_id = self.slot().ok_or(PlayError::NoActiveSlot)?;
        let eq = self
            .core
            .engine
            .slot_eq(slot_id)
            .ok_or(PlayError::SlotNotFound(slot_id))?;
        eq.reset();
        for band in 0..eq.len() {
            self.core.engine.set_master_eq_gain(band, 0.0)?;
        }
        Ok(())
    }

    /// Enable or disable the built-in linear auto-advance handler.
    pub fn set_auto_advance_enabled(&self, enabled: bool) {
        self.core.params.set_auto_advance_enabled(enabled);
    }

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        let clamped = self.core.params.set_crossfade_duration(seconds);
        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(clamped));
    }

    /// Set the default playback rate used by `play()` and `select_item()`.
    pub fn set_default_rate(&self, rate: f32) {
        self.core.params.set_default_rate(rate);
    }

    /// Set EQ gain for a band in dB.
    pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let slot_id = self.slot().ok_or(PlayError::NoActiveSlot)?;
        let eq = self
            .core
            .engine
            .slot_eq(slot_id)
            .ok_or(PlayError::SlotNotFound(slot_id))?;
        let clamped = eq.set_gain(band, gain_db)?;
        self.core.engine.set_master_eq_gain(band, clamped)
    }

    /// Set muted state.
    pub fn set_muted(&self, muted: bool) {
        self.core.params.set_muted(muted);
        let effective = if muted { 0.0 } else { self.volume() };
        self.apply_effective_volume(effective);
        self.core
            .engine
            .bus()
            .publish(PlayerEvent::MuteChanged { muted });
    }

    /// Set prefetch lead time in seconds.
    ///
    /// Canonical owner of this knob is `kithara_queue::Queue` — prefer
    /// `Queue::set_prefetch_duration` for queue-driven applications.
    /// Controls how early the next queued item is loaded into the processor
    /// before EOF. Independent of crossfade activation.
    pub fn set_prefetch_duration(&self, seconds: f32) {
        let clamped = self.core.params.set_prefetch_duration(seconds);
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(clamped));
    }

    /// Pump audio backend/runtime state.
    pub fn tick(&self) -> Result<(), PlayError> {
        self.core.engine.tick()
    }

    /// Set playback rate.
    ///
    /// Stores the speed in the shared time-stretch controls (the single source
    /// of truth, read each chunk by the effect chain) and propagates it via
    /// `PlayerCmd::SetPlaybackRate`. Values below 0.01 are clamped to 0.01.
    pub fn set_rate(&self, rate: f32) {
        let clamped = self.core.params.set_rate(rate);
        self.core.timestretch.set_speed(clamped);
        let _ = self.send_to_slot(PlayerCmd::SetPlaybackRate(clamped));
        self.core
            .engine
            .bus()
            .publish(PlayerEvent::RateChanged { rate: clamped });
    }

    /// Set volume, clamped to `0.0..=1.0`.
    pub fn set_volume(&self, volume: f32) {
        let clamped = self.core.params.set_volume(volume);
        if !self.is_muted() {
            self.apply_effective_volume(clamped);
        }
        self.core
            .engine
            .bus()
            .publish(PlayerEvent::VolumeChanged { volume: clamped });
    }
}
