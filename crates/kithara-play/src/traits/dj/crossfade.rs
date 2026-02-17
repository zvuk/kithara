use std::time::Duration;

use kithara_platform::{MaybeSend, MaybeSync};

use crate::{error::PlayError, types::SlotId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CrossfadeCurve {
    #[default]
    EqualPower,
    Linear,
    SCurve,
    ConstantPower,
    FastFadeIn,
    FastFadeOut,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CrossfadeConfig {
    pub duration: Duration,
    pub curve: CrossfadeCurve,
    pub beat_aligned: bool,
    pub cut_incoming_at: f32,
    pub cut_outgoing_at: f32,
}

impl Default for CrossfadeConfig {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(5),
            curve: CrossfadeCurve::EqualPower,
            beat_aligned: false,
            cut_incoming_at: 0.0,
            cut_outgoing_at: 1.0,
        }
    }
}

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = CrossfadeControllerMock)
)]
pub trait CrossfadeController: MaybeSend + MaybeSync + 'static {
    fn start(&self, from: SlotId, to: SlotId, config: CrossfadeConfig) -> Result<(), PlayError>;

    fn cancel(&self) -> Result<(), PlayError>;

    fn is_active(&self) -> bool;

    fn progress(&self) -> f32;

    fn remaining(&self) -> Duration;

    fn source_slot(&self) -> Option<SlotId>;

    fn target_slot(&self) -> Option<SlotId>;

    fn set_curve(&self, curve: CrossfadeCurve);

    fn set_duration(&self, duration: Duration);
}
