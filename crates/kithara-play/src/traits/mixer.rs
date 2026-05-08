use kithara_platform::{MaybeSend, MaybeSync};

use crate::{error::PlayError, types::SlotId};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

#[kithara::mock(api = MixerMock)]
pub trait Mixer: MaybeSend + MaybeSync + 'static {
    fn channel_gain(&self, slot: SlotId) -> Option<f32>;

    fn channel_mute(&self, slot: SlotId) -> Option<bool>;

    fn channel_pan(&self, slot: SlotId) -> Option<f32>;

    fn channel_peak_level(&self, slot: SlotId) -> Option<(f32, f32)>;

    fn channel_solo(&self, slot: SlotId) -> Option<bool>;

    fn crossfader_assign_center(&self, slot: SlotId) -> Result<(), PlayError>;

    fn crossfader_assign_left(&self, slot: SlotId) -> Result<(), PlayError>;

    fn crossfader_assign_right(&self, slot: SlotId) -> Result<(), PlayError>;

    fn crossfader_position(&self) -> f32;

    fn eq_band_count(&self) -> usize;

    fn eq_gain(&self, slot: SlotId, band: usize) -> Option<f32>;

    fn master_gain(&self) -> f32;

    fn master_peak_level(&self) -> (f32, f32);

    fn reset_eq(&self, slot: SlotId) -> Result<(), PlayError>;

    fn set_channel_gain(&self, slot: SlotId, gain: f32) -> Result<(), PlayError>;

    fn set_channel_mute(&self, slot: SlotId, muted: bool) -> Result<(), PlayError>;

    fn set_channel_pan(&self, slot: SlotId, pan: f32) -> Result<(), PlayError>;

    fn set_channel_solo(&self, slot: SlotId, solo: bool) -> Result<(), PlayError>;

    fn set_crossfader_position(&self, position: f32);

    fn set_eq_gain(&self, slot: SlotId, band: usize, db: f32) -> Result<(), PlayError>;

    fn set_master_gain(&self, gain: f32);
}
