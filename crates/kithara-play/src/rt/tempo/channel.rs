use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use triple_buffer::{Input, Output, triple_buffer};

use super::{
    arm::TempoArm, observation::TempoParticipantObservation, protocol::TempoParticipantCommand,
    stage::TempoStage,
};

const COMMAND_CAPACITY: usize = 8;

pub(crate) struct TempoParticipantEndpoint {
    command_rx: HeapCons<TempoParticipantCommand>,
    observation: Input<TempoParticipantObservation>,
}

impl TempoParticipantEndpoint {
    pub(crate) fn receive(&mut self) -> Option<TempoParticipantCommand> {
        self.command_rx.try_pop()
    }

    pub(crate) fn publish(&mut self, observation: TempoParticipantObservation) {
        self.observation.write(observation);
    }
}

pub(crate) struct TempoParticipantControl {
    command_tx: HeapProd<TempoParticipantCommand>,
    observation: Output<TempoParticipantObservation>,
}

impl TempoParticipantControl {
    pub(crate) fn abort(&mut self, revision: u64) -> Result<(), ()> {
        self.command_tx
            .try_push(TempoParticipantCommand::Abort(revision))
            .map_err(|_| ())
    }

    pub(crate) fn arm(&mut self, arm: TempoArm) -> Result<(), ()> {
        self.command_tx
            .try_push(TempoParticipantCommand::Arm(arm))
            .map_err(|_| ())
    }

    pub(crate) fn command_lane_available(&self) -> bool {
        !self.command_tx.is_full()
    }

    pub(crate) fn observation(&mut self) -> TempoParticipantObservation {
        *self.observation.read()
    }

    pub(crate) fn stage(&mut self, stage: TempoStage) -> Result<(), ()> {
        self.command_tx
            .try_push(TempoParticipantCommand::Stage(stage))
            .map_err(|_| ())
    }
}

pub(crate) fn tempo_participant_channel() -> (TempoParticipantEndpoint, TempoParticipantControl) {
    let (command_tx, command_rx) = HeapRb::new(COMMAND_CAPACITY).split();
    let (observation, output) = triple_buffer(&TempoParticipantObservation::default());
    (
        TempoParticipantEndpoint {
            command_rx,
            observation,
        },
        TempoParticipantControl {
            command_tx,
            observation: output,
        },
    )
}
