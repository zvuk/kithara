use kithara_platform::{MaybeSend, MaybeSync};

use crate::{error::PlayError, types::SlotId};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

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

#[kithara::mock(api = DjEffectMock)]
pub trait DjEffect: MaybeSend + MaybeSync + 'static {
    fn is_active(&self, slot: SlotId) -> bool;

    fn kind(&self) -> DjEffectKind;

    fn parameter(&self, slot: SlotId, name: &str) -> Option<f32>;

    fn parameter_names(&self) -> Vec<String>;

    fn reset(&self, slot: SlotId) -> Result<(), PlayError>;

    fn set_active(&self, slot: SlotId, active: bool) -> Result<(), PlayError>;

    fn set_parameter(&self, slot: SlotId, name: &str, value: f32) -> Result<(), PlayError>;

    fn set_wet_dry(&self, slot: SlotId, mix: f32) -> Result<(), PlayError>;

    fn wet_dry(&self, slot: SlotId) -> Option<f32>;
}
