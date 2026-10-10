use kithara::{
    platform::{atomic::RelaxedAtomicF32, sync::Mutex},
    play::CrossfadeSettings,
    queue::{ActionAtItemEnd, PlaybackOrder, RepeatMode},
};

use super::{WasmInner, core::try_send};
use crate::{
    types::{FfiActionAtItemEnd, FfiCrossfadeSettings, FfiError, FfiPlaybackOrder, FfiRepeatMode},
    web::{bridge::WorkerBridge, commands::WorkerCmd},
};
mod consts {
    pub(super) const EQ_BANDS: usize = 10;
}

/// Main-thread settings mirrored into the worker-owned Queue.
/// Policy, volume and mute updates commit locally only after the command is
/// accepted; EQ retains its immediate local-update semantics.
pub(super) struct Settings {
    playing_rate: RelaxedAtomicF32,
    volume: RelaxedAtomicF32,
    action_at_item_end: Mutex<FfiActionAtItemEnd>,
    crossfade_settings: Mutex<FfiCrossfadeSettings>,
    muted: Mutex<bool>,
    playback_order: Mutex<FfiPlaybackOrder>,
    repeat_mode: Mutex<FfiRepeatMode>,
    eq_gains: [RelaxedAtomicF32; consts::EQ_BANDS],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: RelaxedAtomicF32::new(Self::DEFAULT_VOLUME),
            crossfade_settings: Mutex::new(CrossfadeSettings::default().into()),
            playing_rate: RelaxedAtomicF32::new(Self::DEFAULT_PLAYING_RATE),
            repeat_mode: Mutex::new(FfiRepeatMode::Off),
            playback_order: Mutex::new(FfiPlaybackOrder::Sequential),
            action_at_item_end: Mutex::new(FfiActionAtItemEnd::Advance),
            muted: Mutex::default(),
            eq_gains: [const { RelaxedAtomicF32::new(0.0) }; consts::EQ_BANDS],
        }
    }
}

impl Settings {
    const DEFAULT_PLAYING_RATE: f32 = 1.0;
    const DEFAULT_VOLUME: f32 = 0.5;

    pub(super) fn action_at_item_end(&self) -> FfiActionAtItemEnd {
        *self.action_at_item_end.lock()
    }

    pub(super) fn crossfade_settings(&self) -> FfiCrossfadeSettings {
        *self.crossfade_settings.lock()
    }

    pub(super) fn eq_band_count(&self) -> u32 {
        let n = self.eq_gains.len();
        u32::try_from(n).unwrap_or_else(|_| {
            tracing::error!(eq_band_count = n, "BUG: EQ band count exceeds u32::MAX");
            0
        })
    }

    pub(super) fn eq_gain(&self, band: u32) -> f32 {
        self.eq_gains
            .get(band as usize)
            .map_or(0.0, RelaxedAtomicF32::load)
    }

    pub(super) fn is_muted(&self) -> bool {
        *self.muted.lock()
    }

    pub(super) fn playback_order(&self) -> FfiPlaybackOrder {
        *self.playback_order.lock()
    }

    pub(super) fn repeat_mode(&self) -> FfiRepeatMode {
        *self.repeat_mode.lock()
    }

    pub(super) fn reset_eq(&self, bridge: &WorkerBridge) -> Result<(), FfiError> {
        for g in &self.eq_gains {
            g.store(0.0);
        }
        try_send(bridge, WorkerCmd::ResetEq)
    }

    pub(super) fn set_action_at_item_end(
        &self,
        bridge: &WorkerBridge,
        action: FfiActionAtItemEnd,
    ) -> Result<(), FfiError> {
        let typed: ActionAtItemEnd = action.try_into()?;
        try_send(bridge, WorkerCmd::SetActionAtItemEnd(typed))?;
        *self.action_at_item_end.lock() = action;
        Ok(())
    }

    pub(super) fn set_crossfade_settings(
        &self,
        bridge: &WorkerBridge,
        settings: FfiCrossfadeSettings,
    ) -> Result<(), FfiError> {
        let typed: CrossfadeSettings = settings.try_into()?;
        try_send(bridge, WorkerCmd::SetCrossfade(typed))?;
        *self.crossfade_settings.lock() = settings;
        Ok(())
    }

    pub(super) fn set_eq_gain(
        &self,
        bridge: &WorkerBridge,
        band: u32,
        gain_db: f32,
    ) -> Result<(), FfiError> {
        if let Some(slot) = self.eq_gains.get(band as usize) {
            slot.store(gain_db);
        }
        try_send(bridge, WorkerCmd::SetEqGain { band, gain_db })
    }

    pub(super) fn set_muted(&self, bridge: &WorkerBridge, muted: bool) -> Result<(), FfiError> {
        let volume = if muted { 0.0 } else { self.volume.load() };
        try_send(bridge, WorkerCmd::SetVolume(volume))?;
        *self.muted.lock() = muted;
        Ok(())
    }

    pub(super) fn set_playback_order(
        &self,
        bridge: &WorkerBridge,
        order: FfiPlaybackOrder,
    ) -> Result<(), FfiError> {
        let typed: PlaybackOrder = order.try_into()?;
        try_send(bridge, WorkerCmd::SetPlaybackOrder(typed))?;
        *self.playback_order.lock() = order;
        Ok(())
    }

    pub(super) fn try_set_playing_rate(
        &self,
        bridge: &WorkerBridge,
        rate: f32,
    ) -> Result<(), FfiError> {
        if !rate.is_finite() {
            return Err(FfiError::InvalidArgument {
                reason: "playing rate must be finite".into(),
            });
        }
        let target = rate.max(kithara::play::MIN_SPEED);
        try_send(bridge, WorkerCmd::SetPlayingRate(target))?;
        self.playing_rate.store(target);
        Ok(())
    }

    pub(super) fn set_repeat_mode(
        &self,
        bridge: &WorkerBridge,
        mode: FfiRepeatMode,
    ) -> Result<(), FfiError> {
        let mode = RepeatMode::try_from(mode).map_err(|rejected| FfiError::InvalidArgument {
            reason: format!("repeat mode {rejected:?} is not supported"),
        })?;
        try_send(bridge, WorkerCmd::SetRepeat(mode))?;
        *self.repeat_mode.lock() = mode.into();
        Ok(())
    }

    pub(super) fn set_volume(&self, bridge: &WorkerBridge, volume: f32) -> Result<(), FfiError> {
        if !*self.muted.lock() {
            try_send(bridge, WorkerCmd::SetVolume(volume))?;
        }
        self.volume.store(volume);
        Ok(())
    }

    delegate::delegate! {
        to self.playing_rate {
            #[call(load)]
            pub(super) fn playing_rate(&self) -> f32;
        }
        to self.volume {
            #[call(load)]
            pub(super) fn volume(&self) -> f32;
        }
    }
}

impl WasmInner {
    pub(crate) fn reset_eq(&self) -> Result<(), FfiError> {
        self.settings.reset_eq(&self.bridge)
    }

    pub(crate) fn set_action_at_item_end(
        &self,
        action: FfiActionAtItemEnd,
    ) -> Result<(), FfiError> {
        self.settings.set_action_at_item_end(&self.bridge, action)
    }

    pub(crate) fn set_crossfade_settings(
        &self,
        settings: FfiCrossfadeSettings,
    ) -> Result<(), FfiError> {
        self.settings.set_crossfade_settings(&self.bridge, settings)
    }

    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        self.settings.set_eq_gain(&self.bridge, band, gain_db)
    }

    pub(crate) fn set_muted(&self, muted: bool) -> Result<(), FfiError> {
        self.settings.set_muted(&self.bridge, muted)
    }

    pub(crate) fn set_playback_order(&self, order: FfiPlaybackOrder) -> Result<(), FfiError> {
        self.settings.set_playback_order(&self.bridge, order)
    }

    pub(crate) fn try_set_playing_rate(&self, rate: f32) -> Result<(), FfiError> {
        self.settings.try_set_playing_rate(&self.bridge, rate)
    }

    pub(crate) fn set_repeat_mode(&self, mode: FfiRepeatMode) -> Result<(), FfiError> {
        self.settings.set_repeat_mode(&self.bridge, mode)
    }

    pub(crate) fn set_volume(&self, volume: f32) -> Result<(), FfiError> {
        self.settings.set_volume(&self.bridge, volume)
    }

    delegate::delegate! {
        to self.settings {
            pub(crate) fn action_at_item_end(&self) -> FfiActionAtItemEnd;
            pub(crate) fn crossfade_settings(&self) -> FfiCrossfadeSettings;
            pub(crate) fn eq_band_count(&self) -> u32;
            pub(crate) fn eq_gain(&self, band: u32) -> f32;
            pub(crate) fn is_muted(&self) -> bool;
            pub(crate) fn playback_order(&self) -> FfiPlaybackOrder;
            pub(crate) fn playing_rate(&self) -> f32;
            pub(crate) fn repeat_mode(&self) -> FfiRepeatMode;
            pub(crate) fn volume(&self) -> f32;
        }
    }
}
