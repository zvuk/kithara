use firewheel::backend::AudioBackend;

use super::barrier::preparation_error;
use crate::{
    rt::{TempoArm, TempoParticipantObservation, TempoStage},
    session::{
        protocol::{SessionError, TransportPreparationFailure},
        state::{
            SessionState, TransportParticipantId, TransportParticipantLedger, TransportPreparation,
        },
    },
};

pub(super) fn participant_roster<B: AudioBackend>(
    state: &SessionState<B>,
) -> Vec<TransportParticipantLedger> {
    state
        .players
        .iter()
        .flat_map(|player| {
            player.slots.iter().map(|slot| TransportParticipantLedger {
                id: TransportParticipantId {
                    player_id: player.player_id,
                    slot_id: slot.slot_id,
                },
                ready: None,
            })
        })
        .collect()
}

pub(super) fn participant_observations<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Vec<(TransportParticipantId, TempoParticipantObservation)> {
    state
        .players
        .iter_mut()
        .flat_map(|player| {
            player.slots.iter_mut().map(|slot| {
                (
                    TransportParticipantId {
                        player_id: player.player_id,
                        slot_id: slot.slot_id,
                    },
                    slot.tempo.observation(),
                )
            })
        })
        .collect()
}

pub(super) fn ensure_participant_roster<B: AudioBackend>(
    state: &SessionState<B>,
    preparation: &TransportPreparation,
) -> Result<(), SessionError> {
    let current = participant_roster(state);
    if current.len() == preparation.participants.len()
        && current
            .iter()
            .zip(&preparation.participants)
            .all(|(current, expected)| current.id == expected.id)
    {
        return Ok(());
    }
    let Some(participant) = preparation.participants.first().or_else(|| current.first()) else {
        return Err(SessionError::TransportSync(
            "transport participant roster is unexpectedly empty".into(),
        ));
    };
    Err(preparation_error(
        preparation.candidate.revision(),
        participant.id,
        TransportPreparationFailure::MembershipChanged,
    ))
}

pub(super) fn queue_stage<B: AudioBackend>(
    state: &mut SessionState<B>,
    stage: TempoStage,
) -> Result<(), SessionError> {
    for player in &state.players {
        for slot in &player.slots {
            if !slot.tempo.command_lane_available() {
                return Err(preparation_error(
                    stage.revision(),
                    TransportParticipantId {
                        player_id: player.player_id,
                        slot_id: slot.slot_id,
                    },
                    TransportPreparationFailure::CommandLaneUnavailable,
                ));
            }
        }
    }
    for player in &mut state.players {
        for slot in &mut player.slots {
            let participant_id = TransportParticipantId {
                player_id: player.player_id,
                slot_id: slot.slot_id,
            };
            slot.tempo.stage(stage).map_err(|()| {
                preparation_error(
                    stage.revision(),
                    participant_id,
                    TransportPreparationFailure::CommandLaneUnavailable,
                )
            })?;
        }
    }
    Ok(())
}

pub(super) fn queue_arm<B: AudioBackend>(
    state: &mut SessionState<B>,
    preparation: &TransportPreparation,
) -> Result<(), SessionError> {
    let revision = preparation.candidate.revision();
    for (slot, participant) in state
        .players
        .iter()
        .flat_map(|player| &player.slots)
        .zip(&preparation.participants)
    {
        if !slot.tempo.command_lane_available() || participant.ready.is_none() {
            return Err(preparation_error(
                revision,
                participant.id,
                TransportPreparationFailure::CommandLaneUnavailable,
            ));
        }
    }
    for (slot, participant) in state
        .players
        .iter_mut()
        .flat_map(|player| &mut player.slots)
        .zip(&preparation.participants)
    {
        let ready = participant.ready.ok_or_else(|| {
            preparation_error(
                revision,
                participant.id,
                TransportPreparationFailure::StaleRevision,
            )
        })?;
        let arm = TempoArm::new(
            revision,
            ready.membership_epoch,
            ready.boundary.render,
            ready.boundary.presentation,
        );
        slot.tempo.arm(arm).map_err(|()| {
            preparation_error(
                revision,
                participant.id,
                TransportPreparationFailure::CommandLaneUnavailable,
            )
        })?;
    }
    Ok(())
}

pub(super) fn queue_participant_abort<B: AudioBackend>(
    state: &mut SessionState<B>,
    participants: &[TransportParticipantLedger],
    revision: u64,
) -> Result<(), SessionError> {
    for (slot, participant) in state
        .players
        .iter()
        .flat_map(|player| &player.slots)
        .zip(participants)
    {
        if !slot.tempo.command_lane_available() {
            return Err(preparation_error(
                revision,
                participant.id,
                TransportPreparationFailure::CommandLaneUnavailable,
            ));
        }
    }
    for (slot, participant) in state
        .players
        .iter_mut()
        .flat_map(|player| &mut player.slots)
        .zip(participants)
    {
        slot.tempo.abort(revision).map_err(|()| {
            preparation_error(
                revision,
                participant.id,
                TransportPreparationFailure::CommandLaneUnavailable,
            )
        })?;
    }
    Ok(())
}
