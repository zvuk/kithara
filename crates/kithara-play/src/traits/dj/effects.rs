use kithara_platform::{MaybeSend, MaybeSync};

use crate::{error::PlayError, types::SlotId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DjEffectKind {
    Filter,
    Echo,
    Reverb,
    Flanger,
    Phaser,
    Brake,
    Spinback,
    Backspin,
    Gate,
    BitCrusher,
}

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = DjEffectMock)
)]
pub trait DjEffect: MaybeSend + MaybeSync + 'static {
    fn kind(&self) -> DjEffectKind;

    fn is_active(&self, slot: SlotId) -> bool;

    fn set_active(&self, slot: SlotId, active: bool) -> Result<(), PlayError>;

    fn wet_dry(&self, slot: SlotId) -> Option<f32>;

    fn set_wet_dry(&self, slot: SlotId, mix: f32) -> Result<(), PlayError>;

    fn parameter(&self, slot: SlotId, name: &str) -> Option<f32>;

    fn set_parameter(&self, slot: SlotId, name: &str, value: f32) -> Result<(), PlayError>;

    fn parameter_names(&self) -> Vec<String>;

    fn reset(&self, slot: SlotId) -> Result<(), PlayError>;
}
