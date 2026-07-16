use firewheel::backend::AudioBackend;

use super::{
    super::stream_shape,
    barrier::preparation_error,
    participant::{participant_roster, queue_participant_abort, queue_stage},
};
use crate::{
    rt::{SessionTransportCommit, TempoStage},
    session::{
        protocol::{SessionError, TransportPreparationFailure},
        state::{
            PendingTransportCommit, SessionState, SessionTransportState,
            TransportParticipantLedger, TransportPreparation,
        },
    },
};

pub(in crate::session::transport) fn begin<B: AudioBackend>(
    state: &mut SessionState<B>,
    candidate: SessionTransportCommit,
    active: SessionTransportCommit,
) -> Result<bool, SessionError> {
    let participants = participant_roster(state);
    if participants.is_empty() {
        return Ok(false);
    }
    let stage = TempoStage::new(
        candidate.revision(),
        Some(active.revision()),
        candidate.tempo(),
        active.is_playing(),
        stream_shape(state)?,
    );
    start_preparation(&mut state.transport, candidate, participants);
    if let Err(error) = queue_stage(state, stage) {
        state.transport.pending = None;
        return Err(error);
    }
    Ok(true)
}

pub(in crate::session) fn ensure_graph_mutation_allowed<B: AudioBackend>(
    state: &SessionState<B>,
) -> Result<(), SessionError> {
    if let Some(pending) = state.transport.pending.as_ref() {
        return Err(SessionError::TransportGraphMutationPending {
            revision: pending.revision(),
        });
    }
    Ok(())
}

pub(in crate::session) fn invalidate_pending<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Option<SessionError> {
    let pending = state.transport.pending.take()?;
    let (participants, revision, transport_scheduled) = match pending {
        PendingTransportCommit::Preparing(preparation)
        | PendingTransportCommit::Arming(preparation) => (
            preparation.participants,
            preparation.candidate.revision(),
            false,
        ),
        PendingTransportCommit::AbortingParticipants {
            participants,
            revision,
            ..
        } => (participants, revision, false),
        PendingTransportCommit::Applying {
            participants,
            revision,
        }
        | PendingTransportCommit::AbortingTransport {
            participants,
            revision,
        } => (participants, revision, true),
    };

    let error = participants.first().map(|participant| {
        preparation_error(
            revision,
            participant.id,
            TransportPreparationFailure::RouteInvalidated,
        )
    });
    if !participants.is_empty() {
        let _ = queue_participant_abort(state, &participants, revision);
    }
    if transport_scheduled
        && let (Some(ctx), Some(control)) =
            (state.ctx.as_mut(), state.render_context_control.as_ref())
    {
        control.queue_abort(ctx, revision);
        let _ = ctx.update();
    }
    state.transport.accepted = state.transport.observed;
    error
}

pub(super) fn start_preparation(
    transport: &mut SessionTransportState,
    candidate: SessionTransportCommit,
    participants: Vec<TransportParticipantLedger>,
) {
    transport.last_revision = candidate.revision();
    transport.pending = Some(PendingTransportCommit::Preparing(TransportPreparation {
        boundary: None,
        candidate,
        participants,
    }));
}
