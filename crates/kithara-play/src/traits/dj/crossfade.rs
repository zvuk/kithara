use kithara_platform::{MaybeSend, MaybeSync, time::Duration};

use crate::{error::PlayError, types::SlotId};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

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
    pub curve: CrossfadeCurve,
    pub duration: Duration,
    pub beat_aligned: bool,
    pub cut_incoming_at: f32,
    pub cut_outgoing_at: f32,
}

impl Default for CrossfadeConfig {
    fn default() -> Self {
        Self {
            curve: CrossfadeCurve::default(),
            duration: Duration::from_secs(5),
            beat_aligned: false,
            cut_incoming_at: 0.0,
            cut_outgoing_at: 1.0,
        }
    }
}

#[kithara::mock(api = CrossfadeControllerMock)]
pub trait CrossfadeController: MaybeSend + MaybeSync + 'static {
    fn cancel(&self) -> Result<(), PlayError>;

    fn is_active(&self) -> bool;

    fn progress(&self) -> f32;

    fn remaining(&self) -> Duration;

    fn set_curve(&self, curve: CrossfadeCurve);

    fn set_duration(&self, duration: Duration);

    fn source_slot(&self) -> Option<SlotId>;

    fn start(&self, from: SlotId, to: SlotId, config: CrossfadeConfig) -> Result<(), PlayError>;

    fn target_slot(&self) -> Option<SlotId>;
}
