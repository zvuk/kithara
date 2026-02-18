use std::time::Duration;

use derivative::Derivative;
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

#[derive(Clone, Debug, Derivative, PartialEq)]
#[derivative(Default)]
#[non_exhaustive]
pub struct CrossfadeConfig {
    #[derivative(Default(value = "Duration::from_secs(5)"))]
    pub duration: Duration,
    pub curve: CrossfadeCurve,
    pub beat_aligned: bool,
    pub cut_incoming_at: f32,
    #[derivative(Default(value = "1.0"))]
    pub cut_outgoing_at: f32,
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
