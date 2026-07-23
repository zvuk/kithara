use std::sync::atomic::{AtomicBool, Ordering};

use kithara_audio::StretchControls;
use kithara_events::EventBus;
use portable_atomic::AtomicF32;
use tracing::{debug, warn};

use crate::{
    api::{PlayerEvent, SlotId},
    bridge::PlayerCmd,
    error::PlayError,
    player::PlayerConfig,
};

#[derive(Debug)]
pub(crate) struct PlayerParams {
    auto_advance_enabled: AtomicBool,
    crossfade_duration: AtomicF32,
    default_rate: AtomicF32,
    muted: AtomicBool,
    prefetch_duration: AtomicF32,
    rate: AtomicF32,
    volume: AtomicF32,
}

impl PlayerParams {
    pub(crate) const MIN_PLAYBACK_RATE: f32 = 0.01;

    pub(crate) fn auto_advance_enabled(&self) -> bool {
        self.auto_advance_enabled.load(Ordering::Relaxed)
    }

    pub(crate) fn crossfade_duration(&self) -> f32 {
        self.crossfade_duration.load(Ordering::Relaxed)
    }

    pub(crate) fn default_rate(&self) -> f32 {
        self.default_rate.load(Ordering::Relaxed)
    }

    pub(crate) fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub(crate) fn prefetch_duration(&self) -> f32 {
        self.prefetch_duration.load(Ordering::Relaxed)
    }

    pub(crate) fn rate(&self) -> f32 {
        self.rate.load(Ordering::Relaxed)
    }

    pub(crate) fn set_auto_advance_enabled(&self, enabled: bool) {
        self.auto_advance_enabled.store(enabled, Ordering::Relaxed);
    }

    pub(crate) fn set_crossfade_duration(
        &self,
        seconds: f32,
        send: impl FnOnce(PlayerCmd) -> Result<(), PlayError>,
    ) {
        let clamped = seconds.max(0.0);
        self.crossfade_duration.store(clamped, Ordering::Relaxed);
        let _ = send(PlayerCmd::SetFadeDuration(clamped));
    }

    pub(crate) fn set_default_rate(&self, rate: f32) {
        self.default_rate.store(rate, Ordering::Relaxed);
    }

    pub(crate) fn set_muted(
        &self,
        muted: bool,
        slot: Option<SlotId>,
        set_slot_volume: impl FnOnce(SlotId, f32) -> Result<(), PlayError>,
        bus: &EventBus,
    ) {
        self.muted.store(muted, Ordering::Relaxed);
        let effective = if muted { 0.0 } else { self.volume() };
        Self::apply_effective_volume(effective, slot, set_slot_volume);
        bus.publish(PlayerEvent::MuteChanged { muted });
    }

    pub(crate) fn set_paused_rate(&self) {
        self.rate.store(0.0, Ordering::Relaxed);
    }

    pub(crate) fn set_prefetch_duration(
        &self,
        seconds: f32,
        send: impl FnOnce(PlayerCmd) -> Result<(), PlayError>,
    ) {
        let clamped = seconds.max(0.0);
        self.prefetch_duration.store(clamped, Ordering::Relaxed);
        let _ = send(PlayerCmd::SetPrefetchDuration(clamped));
    }

    pub(crate) fn set_rate_value(&self, rate: f32) -> f32 {
        let clamped = rate.max(Self::MIN_PLAYBACK_RATE);
        self.rate.store(clamped, Ordering::Relaxed);
        clamped
    }

    pub(crate) fn set_rate(
        &self,
        rate: f32,
        timestretch: &StretchControls,
        send: impl FnOnce(PlayerCmd) -> Result<(), PlayError>,
        bus: &EventBus,
    ) {
        let clamped = self.set_rate_value(rate);
        timestretch.set_speed(clamped);
        let _ = send(PlayerCmd::SetPlaybackRate(clamped));
        bus.publish(PlayerEvent::RateChanged { rate: clamped });
    }

    pub(crate) fn set_volume(
        &self,
        volume: f32,
        slot: Option<SlotId>,
        set_slot_volume: impl FnOnce(SlotId, f32) -> Result<(), PlayError>,
        bus: &EventBus,
    ) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume.store(clamped, Ordering::Relaxed);
        if !self.is_muted() {
            Self::apply_effective_volume(clamped, slot, set_slot_volume);
        }
        bus.publish(PlayerEvent::VolumeChanged { volume: clamped });
    }

    pub(crate) fn volume(&self) -> f32 {
        self.volume.load(Ordering::Relaxed)
    }

    fn apply_effective_volume(
        volume: f32,
        slot: Option<SlotId>,
        set_slot_volume: impl FnOnce(SlotId, f32) -> Result<(), PlayError>,
    ) {
        if let Some(slot_id) = slot {
            debug!(volume, ?slot_id, "applying effective volume to slot");
            if let Err(e) = set_slot_volume(slot_id, volume) {
                warn!(?e, volume, "failed to set slot volume");
            }
        } else {
            debug!(volume, "apply_effective_volume: no slot allocated yet");
        }
    }
}

impl From<&PlayerConfig> for PlayerParams {
    fn from(config: &PlayerConfig) -> Self {
        Self {
            auto_advance_enabled: AtomicBool::new(config.auto_advance_enabled),
            crossfade_duration: AtomicF32::new(config.crossfade_duration),
            default_rate: AtomicF32::new(config.default_rate),
            muted: AtomicBool::new(false),
            prefetch_duration: AtomicF32::new(config.prefetch_duration.max(0.0)),
            rate: AtomicF32::new(0.0),
            volume: AtomicF32::new(1.0),
        }
    }
}
