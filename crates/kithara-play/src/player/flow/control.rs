use delegate::delegate;
use kithara_events::RouteDescription;

use super::super::core::PlayerImpl;
use crate::{
    api::{RouteChangeReason, SessionEvent, SlotId},
    error::PlayError,
};

impl PlayerImpl {
    delegate! {
        to self.core.params {
            /// Enable or disable the built-in linear auto-advance handler.
            pub fn set_auto_advance_enabled(&self, enabled: bool);
            /// Set the default playback rate used by `play()` and `select_item()`.
            pub fn set_default_rate(&self, rate: f32);
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
        self.core.engine.invalidate_audio_route(reason)?;
        self.core.engine.bus().publish(SessionEvent::RouteChanged {
            reason: RouteChangeReason::Unknown,
            previous_route: RouteDescription::default(),
        });
        Ok(())
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

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        self.core
            .params
            .set_crossfade_duration(seconds, |cmd| self.send_to_slot(cmd));
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
        let slot = self.slot();
        self.core.params.set_muted(
            muted,
            slot,
            |slot, volume| self.core.engine.set_slot_volume(slot, volume),
            self.core.engine.bus(),
        );
    }

    /// Set prefetch lead time in seconds.
    ///
    /// Canonical owner of this knob is `kithara_queue::Queue` — prefer
    /// `Queue::set_prefetch_duration` for queue-driven applications.
    /// Controls how early the next queued item is loaded into the processor
    /// before EOF. Independent of crossfade activation.
    pub fn set_prefetch_duration(&self, seconds: f32) {
        self.core
            .params
            .set_prefetch_duration(seconds, |cmd| self.send_to_slot(cmd));
    }

    /// Set the transport pitch-bend multiplier.
    pub fn set_pitch_bend(&self, bend: f32) {
        self.core
            .params
            .set_pitch_bend(bend, |cmd| self.send_to_slot(cmd));
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
        self.core.params.set_rate(
            rate,
            &self.core.timestretch,
            |cmd| self.send_to_slot(cmd),
            self.core.engine.bus(),
        );
    }

    /// Set volume, clamped to `0.0..=1.0`.
    pub fn set_volume(&self, volume: f32) {
        let slot = self.slot();
        self.core.params.set_volume(
            volume,
            slot,
            |slot, volume| self.core.engine.set_slot_volume(slot, volume),
            self.core.engine.bus(),
        );
    }
}
