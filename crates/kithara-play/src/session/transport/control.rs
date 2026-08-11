use std::num::NonZeroU32;

use firewheel::{FirewheelCtx, backend::AudioBackend, error::UpdateError};
use kithara_audio::{
    SessionAnchor, SessionAnchorCell, SessionBeat, SessionFrame, SourceFrame, bound_rate_supported,
    bound_render_span_frames,
};
use kithara_events::TransportEvent;
use kithara_platform::sync::Arc;
use num_traits::ToPrimitive;

use super::{
    TransportControl,
    commit::{
        SessionTransportCommit, TransportBoundary, TransportCommitResult, TransportCommitStamp,
        TransportObservation,
    },
};
use crate::{
    api::{SessionTransportSnapshot, Tempo, TrackBinding, TransportRevision},
    session::{PlayerId, SessionError, graph::player_index, state::SessionState},
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

#[cfg(test)]
pub(crate) fn seed_committed_transport<B: AudioBackend>(
    state: &mut SessionState<B>,
    tempo: Tempo,
    sample_rate: NonZeroU32,
) -> Result<(), SessionError> {
    let active = SessionTransportCommit::new(tempo, true, TransportRevision::FIRST);
    state.transport.phase = TransportPhase::Stable { active };
    state.transport.ledger = TransportLedger {
        completed: Some(TransportRevision::FIRST),
        last: Some(TransportRevision::FIRST),
        rejected: None,
    };
    let anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::default(),
        tempo.beats_per_second(),
        sample_rate,
    )
    .map_err(|_| SessionError::TransportFrameExhausted)?;
    super::anchor(state)?.publish(anchor);
    Ok(())
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
    let (target_frame, sample_rate) = commit_boundary(state)?;
    ensure_tempo_renderable(state, tempo, target_frame, sample_rate)?;
    let revision = next_revision(state)?;
    let next = SessionTransportCommit::new(
        tempo,
        accepted.is_none_or(|commit| commit.is_playing()),
        revision,
    );
    let stamp =
        TransportCommitStamp::new(state.transport.observed(), next, target_frame, sample_rate);
    schedule_commit(state, next, stamp)
}

pub(crate) fn bind_player<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
    binding: TrackBinding,
    at: SessionBeat,
) -> Result<(), SessionError> {
    let _ = refresh_observation(state)?;
    ensure_no_pending_commit(state)?;
    let tempo = state
        .transport
        .accepted()
        .ok_or(SessionError::TransportNotProcessed)?
        .tempo();
    let sample_rate = state
        .ctx
        .as_ref()
        .and_then(FirewheelCtx::stream_info)
        .map(|info| info.sample_rate)
        .ok_or(SessionError::NoContext)?;
    let index = player_index(state, player_id)?;
    ensure_binding_renderable(player_id, &binding, at, tempo, sample_rate)?;
    state.players[index].binding = Some(binding);
    Ok(())
}

pub(crate) fn unbind_player<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
) -> Result<(), SessionError> {
    let index = player_index(state, player_id)?;
    state.players[index].binding = None;
    Ok(())
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

fn ensure_tempo_renderable<B: AudioBackend>(
    state: &SessionState<B>,
    tempo: Tempo,
    target_frame: SessionFrame,
    sample_rate: NonZeroU32,
) -> Result<(), SessionError> {
    if state.players.iter().all(|player| player.binding.is_none()) {
        return Ok(());
    }
    let previous = state
        .transport
        .observed()
        .ok_or(SessionError::TransportNotProcessed)?;
    let anchor = state
        .transport_control
        .as_ref()
        .and_then(|control| control.anchor().load())
        .ok_or(SessionError::TransportNotProcessed)?;
    let start = if previous.is_playing() {
        anchor
            .beat_at(target_frame)
            .map_err(|_| SessionError::TransportFrameExhausted)?
    } else {
        anchor.beat()
    };
    state
        .players
        .iter()
        .filter_map(|player| {
            player
                .binding
                .as_ref()
                .map(|binding| (player.player_id, binding))
        })
        .try_for_each(|(player_id, binding)| {
            ensure_binding_renderable(player_id, binding, start, tempo, sample_rate)
        })
}

fn ensure_binding_renderable(
    player_id: PlayerId,
    binding: &TrackBinding,
    start: SessionBeat,
    tempo: Tempo,
    sample_rate: NonZeroU32,
) -> Result<(), SessionError> {
    let output_frames =
        bound_render_span_frames().ok_or(SessionError::BoundEngineUnavailable { player_id })?;
    let output_frame = i64::try_from(output_frames)
        .map(SessionFrame::new)
        .map_err(|_| SessionError::TransportFrameExhausted)?;
    let span = SessionAnchor::new(
        SessionFrame::new(0),
        start,
        tempo.beats_per_second(),
        sample_rate,
    )
    .map_err(|reason| SessionError::BoundSpanCoordinate { player_id, reason })?;
    let end = span
        .beat_at(output_frame)
        .map_err(|reason| SessionError::BoundSpanCoordinate { player_id, reason })?;
    let source_start = resolve_source_frame(player_id, binding, start, start, end)?;
    let source_end = resolve_source_frame(player_id, binding, end, start, end)?;
    let output_frames = output_frames
        .to_f64()
        .ok_or(SessionError::TransportFrameExhausted)?;
    let rate = (f64::from(source_end) - f64::from(source_start)).abs() / output_frames;
    match bound_rate_supported(rate) {
        Some(true) => Ok(()),
        Some(false) => Err(SessionError::BoundTempoOutsideEnvelope {
            player_id,
            beats_per_minute: tempo.beats_per_minute(),
            source_frames_per_output: rate,
        }),
        None => Err(SessionError::BoundEngineUnavailable { player_id }),
    }
}

fn resolve_source_frame(
    player_id: PlayerId,
    binding: &TrackBinding,
    beat: SessionBeat,
    start: SessionBeat,
    end: SessionBeat,
) -> Result<SourceFrame, SessionError> {
    binding
        .source_frame_at(beat)
        .map_err(|reason| SessionError::BoundBindingUnavailable { player_id, reason })?
        .ok_or_else(|| SessionError::BoundSpanOutsideMap {
            player_id,
            start_beat: f64::from(start),
            end_beat: f64::from(end),
        })
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
