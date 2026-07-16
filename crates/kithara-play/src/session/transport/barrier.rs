use crate::{
    rt::{TempoParticipantObservation, TempoParticipantStatus},
    session::{
        protocol::{SessionError, TransportPreparationFailure},
        state::{
            TransportBoundary, TransportParticipantId, TransportParticipantLedger,
            TransportParticipantReady, TransportPreparation,
        },
    },
};

pub(super) fn collect_readiness(
    preparation: &mut TransportPreparation,
    observations: &[(TransportParticipantId, TempoParticipantObservation)],
) -> Result<Option<TransportBoundary>, SessionError> {
    let revision = preparation.candidate.revision();
    let mut common_boundary = None;
    let mut ready_updates = Vec::with_capacity(observations.len());
    for (id, observation) in observations {
        let Some(participant_index) = preparation
            .participants
            .iter()
            .position(|participant| participant.id == *id)
        else {
            return Err(preparation_error(
                revision,
                *id,
                TransportPreparationFailure::MembershipChanged,
            ));
        };

        if observation.revision() < revision {
            continue;
        }
        if observation.revision() > revision {
            return Err(preparation_error(
                revision,
                *id,
                TransportPreparationFailure::StaleRevision,
            ));
        }

        match observation.status() {
            TempoParticipantStatus::Rejected(reason) => {
                return Err(preparation_error(revision, *id, reason));
            }
            TempoParticipantStatus::Ready => {
                let Some(proposed_boundary) = observation_boundary(*observation) else {
                    return Err(preparation_error(
                        revision,
                        *id,
                        TransportPreparationFailure::RendererUnavailable,
                    ));
                };
                if common_boundary.is_some_and(|common| common != proposed_boundary) {
                    return Err(preparation_error(
                        revision,
                        *id,
                        TransportPreparationFailure::BoundaryMismatch,
                    ));
                }
                common_boundary = Some(proposed_boundary);
                ready_updates.push((
                    participant_index,
                    TransportParticipantReady {
                        boundary: proposed_boundary,
                        effective_latency_frames: observation.effective_latency_frames(),
                        max_raw_latency_frames: observation.max_raw_latency_frames(),
                        membership_epoch: observation.membership_epoch(),
                        participant_count: observation.participant_count(),
                    },
                ));
            }
            TempoParticipantStatus::Aborted
            | TempoParticipantStatus::Applied
            | TempoParticipantStatus::Armed
            | TempoParticipantStatus::Idle
            | TempoParticipantStatus::Preparing => {}
        }
    }

    if ready_updates.len() == preparation.participants.len() {
        for (participant_index, ready) in ready_updates {
            preparation.participants[participant_index].ready = Some(ready);
        }
        preparation.boundary = common_boundary;
        return Ok(common_boundary);
    }

    preparation.boundary = None;
    for participant in &mut preparation.participants {
        participant.ready = None;
    }
    Ok(None)
}

pub(super) fn collect_armed(
    preparation: &TransportPreparation,
    observations: &[(TransportParticipantId, TempoParticipantObservation)],
) -> Result<bool, SessionError> {
    let revision = preparation.candidate.revision();
    let mut armed = 0;
    for participant in &preparation.participants {
        let Some((_, observation)) = observations.iter().find(|(id, _)| *id == participant.id)
        else {
            return Err(preparation_error(
                revision,
                participant.id,
                TransportPreparationFailure::MembershipChanged,
            ));
        };
        if observation.revision() != revision {
            return Err(preparation_error(
                revision,
                participant.id,
                TransportPreparationFailure::StaleRevision,
            ));
        }
        match observation.status() {
            TempoParticipantStatus::Rejected(reason) => {
                return Err(preparation_error(revision, participant.id, reason));
            }
            TempoParticipantStatus::Armed => {
                let Some(ready) = participant.ready else {
                    return Err(preparation_error(
                        revision,
                        participant.id,
                        TransportPreparationFailure::StaleRevision,
                    ));
                };
                let boundary = observation_boundary(*observation).ok_or_else(|| {
                    preparation_error(
                        revision,
                        participant.id,
                        TransportPreparationFailure::RendererUnavailable,
                    )
                })?;
                if observation.membership_epoch() != ready.membership_epoch
                    || observation.participant_count() != ready.participant_count
                {
                    return Err(preparation_error(
                        revision,
                        participant.id,
                        TransportPreparationFailure::MembershipChanged,
                    ));
                }
                if boundary != ready.boundary {
                    return Err(preparation_error(
                        revision,
                        participant.id,
                        TransportPreparationFailure::BoundaryMismatch,
                    ));
                }
                if observation.max_raw_latency_frames() != ready.max_raw_latency_frames
                    || observation.effective_latency_frames() != ready.effective_latency_frames
                {
                    return Err(preparation_error(
                        revision,
                        participant.id,
                        TransportPreparationFailure::StaleRevision,
                    ));
                }
                armed += 1;
            }
            TempoParticipantStatus::Idle
            | TempoParticipantStatus::Preparing
            | TempoParticipantStatus::Ready => {}
            TempoParticipantStatus::Aborted | TempoParticipantStatus::Applied => {
                return Err(preparation_error(
                    revision,
                    participant.id,
                    TransportPreparationFailure::StaleRevision,
                ));
            }
        }
    }
    Ok(armed == preparation.participants.len())
}

pub(super) fn abort_complete(
    participants: &[TransportParticipantLedger],
    revision: u64,
    observations: &[(TransportParticipantId, TempoParticipantObservation)],
) -> bool {
    participants.iter().all(|participant| {
        observations.iter().any(|(id, observation)| {
            *id == participant.id
                && observation.revision() == revision
                && matches!(
                    observation.status(),
                    TempoParticipantStatus::Aborted | TempoParticipantStatus::Rejected(_)
                )
        })
    })
}

pub(super) const fn preparation_error(
    revision: u64,
    participant: TransportParticipantId,
    reason: TransportPreparationFailure,
) -> SessionError {
    SessionError::TransportPreparationRejected {
        player_id: participant.player_id,
        revision,
        slot: participant.slot_id,
        reason,
    }
}

fn observation_boundary(observation: TempoParticipantObservation) -> Option<TransportBoundary> {
    Some(TransportBoundary {
        presentation: observation.presentation_boundary()?,
        render: observation.render_boundary()?,
    })
}
