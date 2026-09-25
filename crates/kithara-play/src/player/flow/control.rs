use kithara_effects::{GainDb, eq::EqBandConfig};
use kithara_test_macros as kithara;
use kithara_warp::StretchControls;
use tracing::warn;

use super::super::core::PlayerRuntime;
use crate::{
    api::{
        InterruptionKind, RouteChangeReason, RouteDescription, SessionDuckingMode, SessionEvent,
        SlotId,
    },
    error::PlayError,
    player::state::phase::PlayerPhaseKind,
};

impl<S> PlayerRuntime<S> {
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

    /// Notify the player that the platform interrupted, or released, the audio
    /// output.
    ///
    /// An interruption stops the output below us — the RT processor is no
    /// longer scheduled, so it can neither observe the interruption nor report
    /// it. The fact enters here and playback state reads it from the session.
    /// Handing the output back is the route-invalidation path, which is what
    /// rebuilds the stream.
    pub fn notify_interruption(&self, kind: InterruptionKind) {
        if matches!(kind, InterruptionKind::Began) {
            let tick = self
                .slot()
                .and_then(|slot| self.core.engine.slot_playback(slot))
                .map_or(0, |shared| {
                    shared
                        .process_count
                        .load(std::sync::atomic::Ordering::Relaxed)
                });
            self.core.engine.suspend_output(tick);
        }
        self.core
            .engine
            .bus()
            .publish(SessionEvent::Interruption { kind });
    }

    /// Reset EQ gains to 0 dB for all bands.
    pub fn reset_eq(&self) -> Result<(), PlayError> {
        let eq = self.core.engine.eq().ok_or(PlayError::EngineNotRunning)?;
        for band in 0..eq.len() {
            self.core.engine.set_master_eq_gain(band, 0.0)?;
        }
        Ok(())
    }

    /// Enable or disable the built-in linear auto-advance handler.
    pub fn set_auto_advance_enabled(&self, enabled: bool) {
        self.core.config.set_auto_advance_enabled(enabled);
    }

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        if let Err(error) = self.try_set_crossfade_duration(seconds) {
            warn!(?error, seconds, "crossfade duration update rejected");
        }
    }

    /// Submit a crossfade duration to the active slot, or retain it for the next slot.
    ///
    /// # Errors
    /// Returns a slot command admission error without changing the retained value.
    pub(crate) fn try_set_crossfade_duration(&self, seconds: f32) -> Result<(), PlayError> {
        self.core
            .config
            .set_crossfade_duration(seconds, |cmd| self.send_to_slot(cmd))
    }

    /// Set the playback rate used by `play()` and `select_item()`, and apply it
    /// as a target to playback that is already running.
    ///
    /// While paused the live rate is 0.0 and must stay there — a rate change is
    /// not a resume. The new value takes effect on the next `play()`.
    pub fn set_default_rate(&self, rate: f32) {
        let target = self.core.config.set_default_rate(rate);
        self.core.config.warp.stretch().set_speed(target);
        if self.phase_kind() == PlayerPhaseKind::Playing {
            self.set_rate(target);
        }
    }

    /// Set EQ gain for a band in dB.
    pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let gain_db = GainDb::from(gain_db);
        self.core
            .engine
            .set_master_eq_gain(band, f32::from(gain_db))
    }

    /// Set muted state.
    pub fn set_muted(&self, muted: bool) {
        let slot = self.slot();
        if let Err(error) = self.core.config.set_muted(
            muted,
            slot,
            |slot, volume| self.core.engine.set_slot_volume(slot, volume),
            self.core.engine.bus(),
        ) {
            warn!(?error, muted, "mute update rejected");
        }
    }

    /// Set prefetch lead time in seconds.
    ///
    /// Canonical owner of this knob is `kithara_queue::Queue` — prefer
    /// `Queue::set_prefetch_duration` for queue-driven applications.
    /// Controls how early the next queued item is loaded into the processor
    /// before EOF. Independent of crossfade activation.
    pub fn set_prefetch_duration(&self, seconds: f32) {
        if let Err(error) = self.try_set_prefetch_duration(seconds) {
            warn!(?error, seconds, "prefetch duration update rejected");
        }
    }

    /// Submit a prefetch duration to the active slot, or retain it for the next slot.
    ///
    /// # Errors
    /// Returns a slot command admission error without changing the retained value.
    pub(crate) fn try_set_prefetch_duration(&self, seconds: f32) -> Result<(), PlayError> {
        self.core
            .config
            .set_prefetch_duration(seconds, |cmd| self.send_to_slot(cmd))
    }

    /// Set the requested rate target, clamped to
    /// [`kithara_warp::StretchControls::MIN_SPEED`].
    pub fn set_rate(&self, rate: f32) {
        let target = rate.max(StretchControls::MIN_SPEED);
        let revision = self.core.config.warp.stretch().set_speed(target);
        let snapshot = self
            .slot()
            .and_then(|slot| self.core.engine.slot_render_snapshot(slot));
        if let Some(snapshot) = snapshot {
            kithara::probe_event!(
                rate_requested,
                request_revision = revision,
                target_rate_bits = target.to_bits(),
                session_epoch = u64::from(snapshot.context().output().session_epoch()),
                transport_revision = snapshot
                    .context()
                    .output()
                    .transport_revision()
                    .map_or(0, u64::from),
                session_frame = i64::from(snapshot.context().output().output_frames().end)
            );
        }
        self.core.config.worker.wake();
    }

    /// Set volume, clamped to `0.0..=1.0`.
    pub fn set_volume(&self, volume: f32) {
        let slot = self.slot();
        if let Err(error) = self.core.config.set_volume(
            volume,
            slot,
            |slot, volume| self.core.engine.set_slot_volume(slot, volume),
            self.core.engine.bus(),
        ) {
            warn!(?error, volume, "volume update rejected");
        }
    }

    delegate::delegate! {
        to self.core.engine {
            /// Replaces the master EQ layout and gains without releasing the running slot.
            ///
            /// # Errors
            /// Returns a session graph error when a running player's EQ node cannot be
            /// replaced.
            #[call(set_master_eq_layout)]
            pub fn set_eq_layout(&self, layout: Vec<EqBandConfig>) -> Result<(), PlayError>;
            /// Lower or restore the whole session output under a competing sound.
            pub fn set_session_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError>;
            /// Pump audio backend/runtime state.
            pub fn tick(&self) -> Result<(), PlayError>;
        }
    }
}
