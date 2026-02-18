use derivative::Derivative;
use kithara_platform::{MaybeSend, MaybeSync};

use crate::{
    error::PlayError,
    types::{EqBand, SlotId},
};

#[derive(Clone, Copy, Debug, Derivative, PartialEq)]
#[derivative(Default)]
#[non_exhaustive]
pub struct EqConfig {
    #[derivative(Default(value = "200.0"))]
    pub low_freq: f32,
    #[derivative(Default(value = "1000.0"))]
    pub mid_freq: f32,
    #[derivative(Default(value = "5000.0"))]
    pub high_freq: f32,
    #[derivative(Default(value = "0.707"))]
    pub low_q: f32,
    #[derivative(Default(value = "0.707"))]
    pub mid_q: f32,
    #[derivative(Default(value = "0.707"))]
    pub high_q: f32,
}

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = EqualizerMock)
)]
pub trait Equalizer: MaybeSend + MaybeSync + 'static {
    fn gain(&self, slot: SlotId, band: EqBand) -> Option<f32>;

    fn set_gain(&self, slot: SlotId, band: EqBand, db: f32) -> Result<(), PlayError>;

    fn is_kill(&self, slot: SlotId, band: EqBand) -> Option<bool>;

    fn set_kill(&self, slot: SlotId, band: EqBand, kill: bool) -> Result<(), PlayError>;

    fn reset(&self, slot: SlotId) -> Result<(), PlayError>;

    fn config(&self) -> EqConfig;

    fn set_config(&self, config: EqConfig) -> Result<(), PlayError>;
}
