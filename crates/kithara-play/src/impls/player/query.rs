use std::sync::atomic::Ordering;

use tracing::{debug, warn};

use super::core::PlayerImpl;
use crate::{
    error::PlayError,
    events::PlayerEvent,
    impls::{player_processor::PlayerCmd, shared_player_state::PlaybackSnapshot},
    traits::engine::Engine,
    types::SlotId,
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

    /// ABR handle of the currently loaded item, if any.
    ///
    /// Reads the stash populated by `enqueue_to_processor` — stays valid for
    /// the whole life of the track, including after `items[idx]` has been
    /// emptied by the load handoff.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.phase.lock().abr_handle()
    }

    /// Single coherent read of the active slot's live playback scalars.
    ///
    /// `None` when no slot is allocated. The standalone `position_seconds`
    /// / `duration_seconds` / `is_playing` / `buffered_seconds` getters are
    /// thin derivations of this snapshot — one shared read primitive.
    pub fn playback_snapshot(&self) -> Option<PlaybackSnapshot> {
        let slot_id = self.slot()?;
        Some(self.core.engine.slot_shared_state(slot_id)?.snapshot())
    }

    /// Current media duration in seconds.
    ///
    /// Returns `None` while duration is unknown — the engine sets the shared
    /// atomic from the demuxer once mvhd / fmt-equivalent metadata is parsed.
    /// The atomic's default `0.0` conflates "unknown" with "empty track";
    /// callers that distinguish (e.g. `seek_seconds`'s `target >= dur` check,
    /// queue auto-advance) need the `None` to avoid false-EOF on a freshly-
    /// loaded track whose demuxer has not yet seen the metadata box.
    pub fn duration_seconds(&self) -> Option<f64> {
        let dur = self.playback_snapshot()?.duration;
        (dur > 0.0).then_some(dur)
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

    /// Get EQ gain for a band in dB.
    pub fn eq_gain(&self, band: usize) -> Option<f32> {
        let slot_id = self.slot()?;
        self.core
            .engine
            .slot_eq(slot_id)
            .and_then(|eq| eq.gain(band))
    }

    /// Returns `true` if the player is in playing state.
    pub fn is_playing(&self) -> bool {
        self.playback_snapshot().is_some_and(|s| s.playing)
    }

    /// Current playback position in seconds.
    pub fn position_seconds(&self) -> Option<f64> {
        Some(self.playback_snapshot()?.position)
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

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        let clamped = seconds.max(0.0);
        self.core
            .crossfade_duration
            .store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(clamped));
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
}
