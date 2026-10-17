use kithara::{abr::AbrMode, queue::RepeatMode};

use super::{NativeInner, PeakBitrate};
use crate::types::{
    FfiAbrMode, FfiActionAtItemEnd, FfiCrossfadeSettings, FfiError, FfiPlaybackOrder, FfiRepeatMode,
};
impl NativeInner {
    pub(crate) fn eq_band_count(&self) -> u32 {
        let n = self.queue.eq_band_count();
        u32::try_from(n).unwrap_or_else(|_| {
            tracing::error!(eq_band_count = n, "BUG: EQ band count exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn eq_gain(&self, band: u32) -> f32 {
        self.queue.eq_gain(band as usize).unwrap_or(0.0)
    }

    pub(crate) fn set_abr_mode(&self, mode: FfiAbrMode) {
        let Some(handle) = self.queue.current_abr_handle() else {
            return;
        };
        let abr_mode = match mode {
            FfiAbrMode::Auto => AbrMode::Auto(None),
            FfiAbrMode::Manual { variant_index } => AbrMode::manual(variant_index as usize),
        };
        if let Err(err) = handle.set_mode(abr_mode) {
            tracing::warn!(?err, "set_abr_mode rejected by ABR state");
        }
    }

    pub(crate) fn set_action_at_item_end(
        &self,
        action: FfiActionAtItemEnd,
    ) -> Result<(), FfiError> {
        self.queue.set_action_at_item_end(action.try_into()?);
        Ok(())
    }

    pub(crate) fn set_crossfade_settings(
        &self,
        settings: FfiCrossfadeSettings,
    ) -> Result<(), FfiError> {
        self.queue
            .set_crossfade_settings(settings.try_into()?)
            .map_err(FfiError::from)
    }

    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        self.queue
            .set_eq_gain(band as usize, gain_db)
            .map_err(FfiError::from)
    }

    pub(crate) fn set_playback_order(&self, order: FfiPlaybackOrder) -> Result<(), FfiError> {
        self.queue.set_playback_order(order.try_into()?);
        Ok(())
    }

    pub(crate) fn set_repeat_mode(&self, mode: FfiRepeatMode) -> Result<(), FfiError> {
        let mode = RepeatMode::try_from(mode).map_err(|rejected| FfiError::InvalidArgument {
            reason: format!("repeat mode {rejected:?} is not supported"),
        })?;
        self.queue.set_repeat(mode);
        Ok(())
    }

    pub(crate) fn update_peak_bitrate(&self, wifi_bps: f64, cellular_bps: f64) {
        let updated = PeakBitrate {
            cellular_bps,
            wifi_bps,
        };
        *self.peak_bitrate.lock() = updated;
        if let Some(handle) = self.queue.current_abr_handle() {
            handle.set_max_bandwidth_bps(updated.effective_cap());
        }
    }
}
