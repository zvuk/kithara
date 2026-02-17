use kithara_platform::{MaybeSend, MaybeSync};

use crate::{
    error::PlayError,
    types::{EqBand, SlotId},
};

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = MixerMock)
)]
pub trait Mixer: MaybeSend + MaybeSync + 'static {
    // -- master bus --

    fn master_gain(&self) -> f32;

    fn set_master_gain(&self, gain: f32);

    fn master_peak_level(&self) -> (f32, f32);

    // -- channel gain / pan --

    fn channel_gain(&self, slot: SlotId) -> Option<f32>;

    fn set_channel_gain(&self, slot: SlotId, gain: f32) -> Result<(), PlayError>;

    fn channel_pan(&self, slot: SlotId) -> Option<f32>;

    fn set_channel_pan(&self, slot: SlotId, pan: f32) -> Result<(), PlayError>;

    // -- mute / solo --

    fn channel_mute(&self, slot: SlotId) -> Option<bool>;

    fn set_channel_mute(&self, slot: SlotId, muted: bool) -> Result<(), PlayError>;

    fn channel_solo(&self, slot: SlotId) -> Option<bool>;

    fn set_channel_solo(&self, slot: SlotId, solo: bool) -> Result<(), PlayError>;

    // -- metering --

    fn channel_peak_level(&self, slot: SlotId) -> Option<(f32, f32)>;

    // -- per-channel EQ --

    fn eq_gain(&self, slot: SlotId, band: EqBand) -> Option<f32>;

    fn set_eq_gain(&self, slot: SlotId, band: EqBand, db: f32) -> Result<(), PlayError>;

    fn eq_kill(&self, slot: SlotId, band: EqBand) -> Option<bool>;

    fn set_eq_kill(&self, slot: SlotId, band: EqBand, kill: bool) -> Result<(), PlayError>;

    fn reset_eq(&self, slot: SlotId) -> Result<(), PlayError>;

    // -- crossfader (hardware-style) --

    fn crossfader_position(&self) -> f32;

    fn set_crossfader_position(&self, position: f32);

    fn crossfader_assign_left(&self, slot: SlotId) -> Result<(), PlayError>;

    fn crossfader_assign_right(&self, slot: SlotId) -> Result<(), PlayError>;

    fn crossfader_assign_center(&self, slot: SlotId) -> Result<(), PlayError>;
}
