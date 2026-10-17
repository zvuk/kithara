use kithara::{
    play::{
        CrossfadeSettings, DeckMixSettings, DeckMixerConfig, TrackSettings, TrackSettingsChange,
    },
    queue::{ActionAtItemEnd, PlaybackOrder, QueueCommand, QueueSettingsChange, RepeatMode},
};
use kithara_command::When;

use super::WasmInner;
use crate::{
    pools::FfiQueueControl,
    types::{FfiActionAtItemEnd, FfiCrossfadeSettings, FfiError, FfiPlaybackOrder, FfiRepeatMode},
    web::worker,
};

impl WasmInner {
    pub(crate) fn reset_eq(&self) -> Result<(), FfiError> {
        self.submit(QueueCommand::ResetEq)
    }

    pub(crate) fn set_action_at_item_end(
        &self,
        action: FfiActionAtItemEnd,
    ) -> Result<(), FfiError> {
        self.submit(QueueCommand::SetActionAtItemEnd(action.try_into()?))
    }

    pub(crate) fn set_crossfade_settings(
        &self,
        settings: FfiCrossfadeSettings,
    ) -> Result<(), FfiError> {
        let settings: CrossfadeSettings = settings.try_into()?;
        self.submit(QueueCommand::ConfigureQueue(
            QueueSettingsChange::Crossfade(settings),
            When::Next,
        ))
    }

    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        self.submit(QueueCommand::SetEqGain {
            band: band as usize,
            gain_db,
        })
    }

    pub(crate) fn set_muted(&self, muted: bool) -> Result<(), FfiError> {
        self.submit(QueueCommand::SetMuted(muted))
    }

    pub(crate) fn set_playback_order(&self, order: FfiPlaybackOrder) -> Result<(), FfiError> {
        self.submit(QueueCommand::SetPlaybackOrder(order.try_into()?))
    }

    pub(crate) fn set_playing_rate(&self, rate: f32) -> Result<(), FfiError> {
        if !rate.is_finite() {
            return Err(FfiError::InvalidArgument {
                reason: "playing rate must be finite".into(),
            });
        }
        self.submit(QueueCommand::ConfigureTrack(
            TrackSettingsChange::Speed(rate),
            When::Next,
        ))
    }

    pub(crate) fn set_repeat_mode(&self, mode: FfiRepeatMode) -> Result<(), FfiError> {
        let mode = RepeatMode::try_from(mode).map_err(|rejected| FfiError::InvalidArgument {
            reason: format!("repeat mode {rejected:?} is not supported"),
        })?;
        self.submit(QueueCommand::SetRepeat(mode))
    }

    pub(crate) fn set_volume(&self, volume: f32) -> Result<(), FfiError> {
        self.submit(QueueCommand::SetVolume(volume))
    }

    pub(crate) fn action_at_item_end(&self) -> FfiActionAtItemEnd {
        self.bridge
            .queue()
            .map_or_else(
                ActionAtItemEnd::default,
                FfiQueueControl::action_at_item_end,
            )
            .into()
    }

    pub(crate) fn crossfade_settings(&self) -> FfiCrossfadeSettings {
        self.bridge
            .queue()
            .map_or_else(
                || worker::queue_settings().crossfade(),
                FfiQueueControl::crossfade_settings,
            )
            .into()
    }

    pub(crate) fn eq_band_count(&self) -> u32 {
        let count = self.bridge.queue().map_or_else(
            || DeckMixerConfig::default().eq_bands(),
            FfiQueueControl::eq_band_count,
        );
        u32::try_from(count).unwrap_or_else(|_| {
            tracing::error!(eq_band_count = count, "BUG: EQ band count exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn eq_gain(&self, band: u32) -> f32 {
        self.bridge
            .queue()
            .and_then(|queue| queue.eq_gain(band as usize))
            .unwrap_or_default()
    }

    pub(crate) fn is_muted(&self) -> bool {
        self.bridge.queue().is_some_and(FfiQueueControl::is_muted)
    }

    pub(crate) fn playback_order(&self) -> FfiPlaybackOrder {
        self.bridge
            .queue()
            .map_or_else(PlaybackOrder::default, FfiQueueControl::playback_order)
            .into()
    }

    pub(crate) fn playing_rate(&self) -> f32 {
        self.bridge.queue().map_or_else(
            || TrackSettings::default().speed(),
            FfiQueueControl::default_rate,
        )
    }

    pub(crate) fn repeat_mode(&self) -> FfiRepeatMode {
        self.bridge
            .queue()
            .map_or_else(RepeatMode::default, FfiQueueControl::repeat_mode)
            .into()
    }

    pub(crate) fn volume(&self) -> f32 {
        self.bridge.queue().map_or_else(
            || f32::from(DeckMixSettings::default().volume()),
            FfiQueueControl::volume,
        )
    }
}
