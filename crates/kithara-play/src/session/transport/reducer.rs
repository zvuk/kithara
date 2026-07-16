use firewheel::{FirewheelCtx, backend::AudioBackend};

use super::{
    super::{abort_commit, queue_stamp, refresh_observation, update_context},
    barrier::{abort_complete, collect_armed, collect_readiness, preparation_error},
    participant::{
        ensure_participant_roster, participant_observations, queue_arm, queue_participant_abort,
    },
};
use crate::{
    rt::TransportCommitStamp,
    session::{
        protocol::{SessionError, TransportPreparationFailure},
        state::{
            PendingTransportCommit, SessionState, TransportBoundary, TransportParticipantLedger,
            TransportPreparation,
        },
    },
};

pub(in crate::session) fn advance<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Result<(), SessionError> {
    if state.render_context_control.is_none() {
        return Ok(());
    }
    let _ = refresh_observation(state)?;
    let Some(pending) = state.transport.pending.take() else {
        return Ok(());
    };

    match pending {
        PendingTransportCommit::Preparing(mut preparation) => {
            if let Err(error) = ensure_participant_roster(state, &preparation) {
                return reject_preparation(state, preparation, error);
            }
            let observations = participant_observations(state);
            match collect_readiness(&mut preparation, &observations) {
                Ok(Some(boundary)) => {
                    if let Err(error) = ensure_future_boundary(state, &preparation, boundary) {
                        return reject_preparation(state, preparation, error);
                    }
                    if let Err(error) = queue_arm(state, &preparation) {
                        return reject_preparation(state, preparation, error);
                    }
                    state.transport.pending = Some(PendingTransportCommit::Arming(preparation));
                    Ok(())
                }
                Ok(None) => {
                    state.transport.pending = Some(PendingTransportCommit::Preparing(preparation));
                    Ok(())
                }
                Err(error) => reject_preparation(state, preparation, error),
            }
        }
        PendingTransportCommit::Arming(preparation) => {
            if let Err(error) = ensure_participant_roster(state, &preparation) {
                return reject_preparation(state, preparation, error);
            }
            let observations = participant_observations(state);
            match collect_armed(&preparation, &observations) {
                Ok(true) => schedule_prepared(state, preparation),
                Ok(false) => {
                    state.transport.pending = Some(PendingTransportCommit::Arming(preparation));
                    Ok(())
                }
                Err(error) => reject_preparation(state, preparation, error),
            }
        }
        PendingTransportCommit::AbortingParticipants {
            abort_queued,
            participants,
            revision,
        } => {
            let observations = participant_observations(state);
            if abort_complete(&participants, revision, &observations) {
                Ok(())
            } else {
                let result = if abort_queued {
                    Ok(())
                } else {
                    queue_participant_abort(state, &participants, revision)
                };
                state.transport.pending = Some(PendingTransportCommit::AbortingParticipants {
                    abort_queued: abort_queued || result.is_ok(),
                    participants,
                    revision,
                });
                result
            }
        }
        pending @ (PendingTransportCommit::Applying { .. }
        | PendingTransportCommit::AbortingTransport { .. }) => {
            state.transport.pending = Some(pending);
            Ok(())
        }
    }
}

pub(in crate::session::transport) fn abort_after_transport_rejection<B: AudioBackend>(
    state: &mut SessionState<B>,
    participants: Vec<TransportParticipantLedger>,
    revision: u64,
) {
    if participants.is_empty() {
        return;
    }
    let abort_queued = queue_participant_abort(state, &participants, revision).is_ok();
    state.transport.pending = Some(PendingTransportCommit::AbortingParticipants {
        abort_queued,
        participants,
        revision,
    });
}

fn ensure_future_boundary<B: AudioBackend>(
    state: &SessionState<B>,
    preparation: &TransportPreparation,
    boundary: TransportBoundary,
) -> Result<(), SessionError> {
    let now = state
        .ctx
        .as_ref()
        .ok_or(SessionError::NoContext)?
        .audio_clock()
        .samples
        .0;
    if boundary.render.get() > now {
        return Ok(());
    }
    let Some(participant) = preparation.participants.first() else {
        return Err(SessionError::TransportSync(String::from(
            "transport preparation has no participants",
        )));
    };
    Err(preparation_error(
        preparation.candidate.revision(),
        participant.id,
        TransportPreparationFailure::BoundaryExpired,
    ))
}

fn schedule_prepared<B: AudioBackend>(
    state: &mut SessionState<B>,
    preparation: TransportPreparation,
) -> Result<(), SessionError> {
    let revision = preparation.candidate.revision();
    let Some(boundary) = preparation.boundary else {
        return reject_preparation(
            state,
            preparation,
            SessionError::TransportSync(String::from("transport preparation boundary is missing")),
        );
    };
    if let Err(error) = ensure_future_boundary(state, &preparation, boundary) {
        return reject_preparation(state, preparation, error);
    }
    let sample_rate = state
        .ctx
        .as_ref()
        .and_then(FirewheelCtx::stream_info)
        .ok_or(SessionError::NoContext)?
        .sample_rate;
    let stamp = TransportCommitStamp::new(
        state.transport.observed,
        preparation.candidate,
        boundary.render,
        sample_rate,
    );
    if let Err(error) = queue_stamp(state, stamp) {
        return reject_preparation(state, preparation, error);
    }
    let candidate = preparation.candidate;
    let participants = preparation.participants;
    if let Err(error) = update_context(state) {
        let _ = queue_participant_abort(state, &participants, revision);
        abort_commit(state, revision, participants);
        return Err(error);
    }
    state.transport.accepted = Some(candidate);
    state.transport.pending = Some(PendingTransportCommit::Applying {
        participants,
        revision,
    });
    Ok(())
}

fn reject_preparation<B: AudioBackend>(
    state: &mut SessionState<B>,
    preparation: TransportPreparation,
    error: SessionError,
) -> Result<(), SessionError> {
    let revision = preparation.candidate.revision();
    let abort_error = queue_participant_abort(state, &preparation.participants, revision).err();
    state.transport.pending = Some(PendingTransportCommit::AbortingParticipants {
        abort_queued: abort_error.is_none(),
        participants: preparation.participants,
        revision,
    });
    Err(abort_error.unwrap_or(error))
}
