use std::sync::atomic::Ordering;

use tracing::{debug, warn};

use super::super::core::PlayerImpl;
use crate::{
    api::{PlayerEvent, SessionDuckingMode},
    bridge::PlayerCmd,
    engine::EngineImpl,
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

    /// Reset EQ gains to 0 dB for all bands.
    pub fn reset_eq(&self) -> Result<(), PlayError> {
        let slot_id = self
            .slot()
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        let eq = self
            .core
            .engine
            .slot_eq(slot_id)
            .ok_or_else(|| PlayError::Internal("eq state not found".into()))?;
        eq.reset();
        for band in 0..eq.len() {
            self.core.engine.set_master_eq_gain(band, 0.0)?;
        }
        Ok(())
    }

    /// Enable or disable the built-in linear auto-advance handler.
    pub fn set_auto_advance_enabled(&self, enabled: bool) {
        self.core
            .auto_advance_enabled
            .store(enabled, Ordering::Relaxed);
    }

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        let clamped = seconds.max(0.0);
        self.core
            .crossfade_duration
            .store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(clamped));
    }

    /// Set the default playback rate used by `play()` and `select_item()`.
    pub fn set_default_rate(&self, rate: f32) {
        self.core.default_rate.store(rate, Ordering::Relaxed);
    }

    /// Set EQ gain for a band in dB.
    pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let slot_id = self
            .slot()
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        let eq = self
            .core
            .engine
            .slot_eq(slot_id)
            .ok_or_else(|| PlayError::Internal("eq state not found".into()))?;
        let clamped = eq.set_gain(band, gain_db)?;
        self.core.engine.set_master_eq_gain(band, clamped)
    }

    /// Set muted state.
    pub fn set_muted(&self, muted: bool) {
        self.core.muted.store(muted, Ordering::Relaxed);
        let effective = if muted { 0.0 } else { self.volume() };
        self.apply_effective_volume(effective);
        self.core.bus.publish(PlayerEvent::MuteChanged { muted });
    }

    /// Set prefetch lead time in seconds.
    ///
    /// Canonical owner of this knob is `kithara_queue::Queue` — prefer
    /// `Queue::set_prefetch_duration` for queue-driven applications.
    /// Controls how early the next queued item is loaded into the processor
    /// before EOF. Independent of crossfade activation.
    pub fn set_prefetch_duration(&self, seconds: f32) {
        let clamped = seconds.max(0.0);
        self.core
            .prefetch_duration
            .store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(clamped));
    }

    /// Set ducking mode for this player's engine session.
    pub fn set_session_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        EngineImpl::set_session_ducking(mode)
    }

    /// Set playback rate.
    ///
    /// Stores the speed in the shared time-stretch controls (the single source
    /// of truth, read each chunk by the effect chain) and propagates it via
    /// `PlayerCmd::SetPlaybackRate`. Values below 0.01 are clamped to 0.01.
    pub fn set_rate(&self, rate: f32) {
        let clamped = rate.max(Self::MIN_PLAYBACK_RATE);
        self.core.rate.store(clamped, Ordering::Relaxed);
        self.core.config.timestretch.set_speed(clamped);
        let _ = self.send_to_slot(PlayerCmd::SetPlaybackRate(clamped));
        self.core
            .bus
            .publish(PlayerEvent::RateChanged { rate: clamped });
    }

    /// Set volume, clamped to `0.0..=1.0`.
    pub fn set_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.core.volume.store(clamped, Ordering::Relaxed);
        if !self.is_muted() {
            self.apply_effective_volume(clamped);
        }
        self.core
            .bus
            .publish(PlayerEvent::VolumeChanged { volume: clamped });
    }
}
