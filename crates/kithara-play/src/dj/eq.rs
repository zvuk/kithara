use crate::error::PlayError;
use crate::types::{EqBand, SlotId};

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct EqConfig {
    pub low_freq: f32,
    pub mid_freq: f32,
    pub high_freq: f32,
    pub low_q: f32,
    pub mid_q: f32,
    pub high_q: f32,
}

impl Default for EqConfig {
    fn default() -> Self {
        Self {
            low_freq: 200.0,
            mid_freq: 1000.0,
            high_freq: 5000.0,
            low_q: 0.707,
            mid_q: 0.707,
            high_q: 0.707,
        }
    }
}

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = EqualizerMock)
)]
pub trait Equalizer: Send + Sync + 'static {
    fn gain(&self, slot: SlotId, band: EqBand) -> Option<f32>;

    fn set_gain(&self, slot: SlotId, band: EqBand, db: f32) -> Result<(), PlayError>;

    fn is_kill(&self, slot: SlotId, band: EqBand) -> Option<bool>;

    fn set_kill(&self, slot: SlotId, band: EqBand, kill: bool) -> Result<(), PlayError>;

    fn reset(&self, slot: SlotId) -> Result<(), PlayError>;

    fn config(&self) -> EqConfig;

    fn set_config(&self, config: EqConfig) -> Result<(), PlayError>;
}
