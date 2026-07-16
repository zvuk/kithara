use super::{arm::TempoArm, stage::TempoStage};
use crate::api::TransportPreparationFailure;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TempoParticipantStatus {
    Aborted,
    Applied,
    Armed,
    #[default]
    Idle,
    Preparing,
    Ready,
    Rejected(TransportPreparationFailure),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TempoParticipantCommand {
    Abort(u64),
    Arm(TempoArm),
    Stage(TempoStage),
}
