use std::num::NonZeroU32;

use firewheel::{FirewheelCtx, backend::AudioBackend, error::UpdateError};
use kithara_audio::{SessionAnchorCell, SessionBeat, SessionFrame};
use kithara_events::TransportEvent;
use kithara_platform::sync::Arc;

use super::{
    TransportControl,
    commit::{
        SessionTransportCommit, TransportBoundary, TransportCommitResult, TransportCommitStamp,
        TransportObservation,
    },
};
use crate::{
    api::{SessionTransportSnapshot, Tempo, TransportRevision},
    session::{SessionError, state::SessionState},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AbortDelivery {
    Pending,
    Sent,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TransportLedger {
    completed: Option<TransportRevision>,
    last: Option<TransportRevision>,
    rejected: Option<TransportRevision>,
}

/// The revision bookkeeping is the same in every phase, so it lives beside the
/// phase rather than being repeated inside each variant.
#[derive(Debug, Default, fieldwork::Fieldwork)]
#[fieldwork(get, get_mut, vis = "pub(crate)")]
pub(crate) struct SessionTransportState {
    ledger: TransportLedger,
    #[field(skip)]
    phase: TransportPhase,
}

#[derive(Debug, Default)]
enum TransportPhase {
    #[default]
    Unconfigured,
    Stable {
        active: SessionTransportCommit,
    },
    Applying {
        next: SessionTransportCommit,
        previous: Option<SessionTransportCommit>,
    },
    Aborting {
        delivery: AbortDelivery,
        previous: Option<SessionTransportCommit>,
        revision: TransportRevision,
    },
}

impl SessionTransportState {
    /// The commit the caller has already asked for, pending or not.
    pub(crate) fn accepted(&self) -> Option<SessionTransportCommit> {
        match self.phase {
            TransportPhase::Unconfigured => None,
            TransportPhase::Stable { active } => Some(active),
            TransportPhase::Applying { next, .. } => Some(next),
            TransportPhase::Aborting { previous, .. } => previous,
        }
    }

    /// The commit the graph has actually rendered.
    pub(crate) fn observed(&self) -> Option<SessionTransportCommit> {
        match self.phase {
            TransportPhase::Unconfigured => None,
            TransportPhase::Stable { active } => Some(active),
            TransportPhase::Applying { previous, .. }
            | TransportPhase::Aborting { previous, .. } => previous,
        }
    }

    pub(crate) fn pending_revision(&self) -> Option<TransportRevision> {
        match self.phase {
            TransportPhase::Applying { next, .. } => Some(next.revision()),
            TransportPhase::Aborting { revision, .. } => Some(revision),
            TransportPhase::Unconfigured | TransportPhase::Stable { .. } => None,
        }
    }
}

pub(crate) fn set_tempo<B: AudioBackend>(
    state: &mut SessionState<B>,
    tempo: Tempo,
) -> Result<(), SessionError> {
    let _ = refresh_observation(state)?;
    let accepted = state.transport.accepted();
    if accepted.is_some_and(|commit| commit.tempo() == tempo) {
        return Ok(());
    }
    ensure_no_pending_commit(state)?;
    let revision = next_revision(state)?;
    let (target_frame, sample_rate) = commit_boundary(state)?;
    let next = SessionTransportCommit::new(
        tempo,
        accepted.is_none_or(|commit| commit.is_playing()),
        revision,
    );
    let stamp =
        TransportCommitStamp::new(state.transport.observed(), next, target_frame, sample_rate);
    schedule_commit(state, next, stamp)
}

pub(crate) fn set_playing<B: AudioBackend>(
    state: &mut SessionState<B>,
    playing: bool,
) -> Result<(), SessionError> {
    let _ = refresh_observation(state)?;
    let accepted = state
        .transport
        .accepted()
        .ok_or(SessionError::TransportNotProcessed)?;
    if accepted.is_playing() == playing {
        return Ok(());
    }
    ensure_no_pending_commit(state)?;
    let revision = next_revision(state)?;
    let (target_frame, sample_rate) = commit_boundary(state)?;
    let next = SessionTransportCommit::new(accepted.tempo(), playing, revision);
    let stamp =
        TransportCommitStamp::new(state.transport.observed(), next, target_frame, sample_rate);
    schedule_commit(state, next, stamp)
}

pub(crate) fn seek<B: AudioBackend>(
    state: &mut SessionState<B>,
    target: SessionBeat,
) -> Result<(), SessionError> {
    let _ = refresh_observation(state)?;
    let accepted = state
        .transport
        .accepted()
        .ok_or(SessionError::TransportNotProcessed)?;
    ensure_no_pending_commit(state)?;
    let revision = next_revision(state)?;
    let (target_frame, sample_rate) = commit_boundary(state)?;
    let next =
        SessionTransportCommit::relocate(accepted.tempo(), accepted.is_playing(), revision, target);
    let stamp =
        TransportCommitStamp::new(state.transport.observed(), next, target_frame, sample_rate);
    schedule_commit(state, next, stamp)
}

pub(crate) fn snapshot<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Result<SessionTransportSnapshot, SessionError> {
    refresh_observation(state)?
        .snapshot()
        .ok_or(SessionError::TransportNotProcessed)
}

/// The grid decks bind to. Installed with the transport node, so a session
/// that has one at all has it before any deck can ask.
pub(crate) fn anchor<B: AudioBackend>(
    state: &SessionState<B>,
) -> Result<Arc<SessionAnchorCell>, SessionError> {
    state
        .transport_control
        .as_ref()
        .map(TransportControl::anchor)
        .ok_or(SessionError::TransportNotProcessed)
}

fn ensure_no_pending_commit<B: AudioBackend>(state: &SessionState<B>) -> Result<(), SessionError> {
    if state.transport.pending_revision().is_some() {
        return Err(SessionError::TransportNotProcessed);
    }
    Ok(())
}

fn next_revision<B: AudioBackend>(
    state: &SessionState<B>,
) -> Result<TransportRevision, SessionError> {
    state
        .transport
        .ledger()
        .last
        .map_or(Ok(TransportRevision::FIRST), |revision| {
            revision
                .checked_next()
                .ok_or(SessionError::TransportRevisionExhausted)
        })
}

fn schedule_commit<B: AudioBackend>(
    state: &mut SessionState<B>,
    next: SessionTransportCommit,
    stamp: TransportCommitStamp,
) -> Result<(), SessionError> {
    let revision = next.revision();
    queue_stamp(state, stamp)?;
    state.transport.ledger_mut().last = Some(revision);
    if let Err(error) = update_context(state) {
        publish_transport_event(
            state,
            &TransportEvent::Failed {
                revision: Some(u64::from(revision)),
                reason: error.to_string(),
            },
        );
        abort_commit(state, revision)?;
        return Err(error);
    }
    let previous = state.transport.observed();
    state.transport.phase = TransportPhase::Applying { next, previous };
    Ok(())
}

fn commit_boundary<B: AudioBackend>(
    state: &SessionState<B>,
) -> Result<(SessionFrame, NonZeroU32), SessionError> {
    let ctx = state.ctx.as_ref().ok_or(SessionError::NoContext)?;
    let stream_info = ctx.stream_info().ok_or(SessionError::NoContext)?;
    let lead_frames = state
        .transport
        .observed()
        .map_or(0, |_| i64::from(stream_info.max_block_frames.get()));
    let target_frame = ctx
        .audio_clock()
        .samples
        .0
        .checked_add(lead_frames)
        .ok_or(SessionError::TransportFrameExhausted)?;
    Ok((SessionFrame::new(target_frame), stream_info.sample_rate))
}

fn queue_stamp<B: AudioBackend>(
    state: &mut SessionState<B>,
    stamp: TransportCommitStamp,
) -> Result<(), SessionError> {
    let ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    let control = state
        .transport_control
        .as_ref()
        .ok_or_else(|| SessionError::Graph("session transport control is missing".to_owned()))?;
    control.queue_stamp(ctx, stamp);
    Ok(())
}

fn update_context<B: AudioBackend>(state: &mut SessionState<B>) -> Result<(), SessionError> {
    let Err(error) = state.ctx.as_mut().ok_or(SessionError::NoContext)?.update() else {
        return Ok(());
    };
    // The session owns stream restarts; swallowing this into a message would
    // strand the transport behind a stream nobody rearms.
    if matches!(error, UpdateError::StreamStoppedUnexpectedly(_)) {
        state.stream_needs_restart = true;
    }
    Err(SessionError::TransportSync(sync_error_reason(error)))
}

fn abort_commit<B: AudioBackend>(
    state: &mut SessionState<B>,
    revision: TransportRevision,
) -> Result<(), SessionError> {
    let ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    let control = state
        .transport_control
        .as_ref()
        .ok_or_else(|| SessionError::Graph("session transport control is missing".to_owned()))?;
    control.queue_abort(ctx, revision);
    let previous = state.transport.observed();
    state.transport.phase = TransportPhase::Aborting {
        previous,
        revision,
        delivery: AbortDelivery::Pending,
    };
    deliver_abort(state)
}

fn deliver_abort<B: AudioBackend>(state: &mut SessionState<B>) -> Result<(), SessionError> {
    update_context(state)?;
    // Firewheel only flushes queued events while a stream runs, so an update
    // that succeeds on a stopped stream has delivered nothing; leaving the
    // abort `Pending` is what makes the next refresh retry it.
    if !state
        .ctx
        .as_ref()
        .is_some_and(FirewheelCtx::is_audio_stream_running)
    {
        return Ok(());
    }
    if let TransportPhase::Aborting { delivery, .. } = &mut state.transport.phase
        && *delivery == AbortDelivery::Pending
    {
        *delivery = AbortDelivery::Sent;
    }
    Ok(())
}

fn sync_error_reason<E>(error: UpdateError<E>) -> String
where
    E: std::error::Error,
{
    match error {
        UpdateError::MsgChannelFull => "message channel is full".to_owned(),
        UpdateError::GraphCompileError(error) => {
            format!("audio graph compilation failed: {error}")
        }
        UpdateError::StreamStoppedUnexpectedly(Some(error)) => {
            format!("audio stream stopped unexpectedly: {error}")
        }
        UpdateError::StreamStoppedUnexpectedly(None) => {
            "audio stream stopped unexpectedly".to_owned()
        }
    }
}

fn refresh_observation<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Result<TransportObservation, SessionError> {
    let observation = state
        .transport_control
        .as_mut()
        .ok_or_else(|| SessionError::Graph("session transport control is missing".to_owned()))?
        .observation();
    if let Some(completion) = observation.completion() {
        apply_completion(state, completion);
    }
    if matches!(
        state.transport.phase,
        TransportPhase::Aborting {
            delivery: AbortDelivery::Pending,
            ..
        }
    ) {
        deliver_abort(state)?;
    }
    if state.transport.ledger_mut().rejected.take().is_some() {
        return Err(SessionError::TransportCommitRejected);
    }
    Ok(observation)
}

fn apply_completion<B: AudioBackend>(
    state: &mut SessionState<B>,
    completion: TransportCommitResult,
) {
    let revision = completion.revision();
    if state
        .transport
        .ledger()
        .completed
        .is_some_and(|completed| revision <= completed)
    {
        return;
    }
    let observed = state.transport.observed();
    let phase = std::mem::take(&mut state.transport.phase);
    let (active, committed, rejected) = match (completion, phase) {
        (TransportCommitResult::Applied(_), TransportPhase::Applying { next, .. })
            if next.revision() == revision =>
        {
            (Some(next), Some(next), false)
        }
        // The graph is authoritative about what it aborted: if it reports our
        // pending revision, the abort happened whether or not our own delivery
        // bookkeeping had caught up.
        (
            TransportCommitResult::Aborted(_),
            TransportPhase::Aborting {
                previous,
                revision: pending_revision,
                ..
            },
        ) if pending_revision == revision => (previous, None, false),
        _ => (observed, None, true),
    };
    let ledger = state.transport.ledger_mut();
    ledger.completed = Some(revision);
    if rejected {
        ledger.rejected = Some(revision);
    }
    state.transport.phase = active.map_or(TransportPhase::Unconfigured, |active| {
        TransportPhase::Stable { active }
    });
    if let Some(next) = committed {
        publish_transport_commit(state, observed, next);
    } else if rejected {
        publish_transport_event(
            state,
            &TransportEvent::Failed {
                revision: Some(u64::from(revision)),
                reason: "render graph rejected transport commit".to_owned(),
            },
        );
    }
}

fn publish_transport_commit<B: AudioBackend>(
    state: &SessionState<B>,
    previous: Option<SessionTransportCommit>,
    next: SessionTransportCommit,
) {
    for event in transport_events(previous, next).into_iter().flatten() {
        publish_transport_event(state, &event);
    }
}

fn transport_events(
    previous: Option<SessionTransportCommit>,
    next: SessionTransportCommit,
) -> [Option<TransportEvent>; 3] {
    let revision = u64::from(next.revision());
    let tempo = previous
        .is_none_or(|commit| commit.tempo() != next.tempo())
        .then(|| TransportEvent::TempoCommitted {
            revision,
            beats_per_minute: next.tempo().beats_per_minute(),
        });
    let play_state = previous
        .is_none_or(|commit| commit.is_playing() != next.is_playing())
        .then(|| TransportEvent::PlayStateCommitted {
            revision,
            playing: next.is_playing(),
        });
    let seek = match next.boundary() {
        TransportBoundary::Continuous => None,
        TransportBoundary::Relocate(target) => Some(TransportEvent::SeekCommitted {
            position_beats: f64::from(target),
            revision,
        }),
    };
    [tempo, play_state, seek]
}

fn publish_transport_event<B: AudioBackend>(state: &SessionState<B>, event: &TransportEvent) {
    for player in &state.players {
        player.bus.publish(event.clone());
    }
}
