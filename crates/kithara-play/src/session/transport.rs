use std::num::NonZeroU32;

use firewheel::{FirewheelCtx, backend::AudioBackend, error::UpdateError};
use kithara_events::TransportEvent;

use super::{
    PlayerId,
    protocol::{PreparationContext, SessionError},
    render::{
        RenderFrame, SessionTransportCommit, TransportCommitResult, TransportCommitStamp,
        TransportObservation,
    },
    state::{AbortDelivery, SessionState, SessionTransportState},
};
use crate::{
    api::{SessionTransportSnapshot, Tempo, TransportRevision},
    player::node::StreamShape,
};

pub(super) fn set_tempo<B: AudioBackend>(
    state: &mut SessionState<B>,
    tempo: Tempo,
) -> Result<(), SessionError> {
    let _ = refresh_observation(state)?;
    set_tempo_after_refresh(state, tempo)
}

pub(super) fn set_tempo_checked<B: AudioBackend>(
    state: &mut SessionState<B>,
    tempo: Tempo,
    expected_context: PreparationContext,
    player_ids: &[PlayerId],
) -> Result<(), SessionError> {
    let _ = validate_checked_transport(state, expected_context, player_ids)?;
    set_tempo_after_refresh(state, tempo)
}

fn validate_checked_transport<B: AudioBackend>(
    state: &mut SessionState<B>,
    expected_context: PreparationContext,
    player_ids: &[PlayerId],
) -> Result<SessionTransportSnapshot, SessionError> {
    let observation = refresh_observation(state)?;
    let snapshot = observation
        .snapshot()
        .ok_or(SessionError::TransportNotProcessed)?;
    let actual_revision = snapshot.revision();
    if actual_revision != expected_context.transport_revision() {
        return Err(SessionError::TransportRevisionChanged {
            expected: expected_context.transport_revision(),
            actual: actual_revision,
        });
    }
    if stream_shape(state)? != expected_context.shape() {
        return Err(SessionError::TransportStreamShapeChanged);
    }
    if state.roster_revision != expected_context.roster_revision() {
        return Err(SessionError::TransportRosterChanged);
    }
    let mut active: Vec<_> = state
        .players
        .iter()
        .filter(|player| player.started && !player.slots.is_empty())
        .map(|player| player.player_id)
        .collect();
    active.sort_unstable();
    if active != player_ids {
        return Err(SessionError::TransportPlayersChanged {
            expected: player_ids.len(),
            actual: active.len(),
        });
    }
    Ok(snapshot)
}

fn set_tempo_after_refresh<B: AudioBackend>(
    state: &mut SessionState<B>,
    tempo: Tempo,
) -> Result<(), SessionError> {
    if state
        .transport
        .accepted()
        .map(SessionTransportCommit::tempo)
        == Some(tempo)
    {
        return Ok(());
    }
    ensure_no_pending_commit(state)?;
    let revision = next_revision(state)?;
    let (target_frame, sample_rate) = commit_boundary(state)?;
    let next = SessionTransportCommit::new(tempo, true, revision);
    let stamp =
        TransportCommitStamp::new(state.transport.observed(), next, target_frame, sample_rate);

    schedule_commit(state, next, stamp)
}

fn ensure_no_pending_commit<B: AudioBackend>(state: &SessionState<B>) -> Result<(), SessionError> {
    if let Some(revision) = state.transport.pending_revision() {
        return Err(SessionError::TransportCommitPending { revision });
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
                revision: Some(revision.get()),
                reason: error.to_string(),
            },
        );
        abort_commit(state, revision)?;
        return Err(error);
    }

    let previous = state.transport.observed();
    let ledger = *state.transport.ledger();
    state.transport = SessionTransportState::Applying {
        ledger,
        next,
        previous,
    };
    Ok(())
}

fn commit_boundary<B: AudioBackend>(
    state: &SessionState<B>,
) -> Result<(RenderFrame, NonZeroU32), SessionError> {
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
    Ok((RenderFrame::new(target_frame), stream_info.sample_rate))
}

fn queue_stamp<B: AudioBackend>(
    state: &mut SessionState<B>,
    stamp: TransportCommitStamp,
) -> Result<(), SessionError> {
    let ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    let control = state
        .render_context_control
        .as_ref()
        .ok_or_else(|| SessionError::Graph("render context control is missing".into()))?;
    control.queue_stamp(ctx, stamp);
    Ok(())
}

fn update_context<B: AudioBackend>(state: &mut SessionState<B>) -> Result<(), SessionError> {
    state
        .ctx
        .as_mut()
        .ok_or(SessionError::NoContext)?
        .update()
        .map_err(|error| SessionError::TransportSync(sync_error_reason(error)))
}

fn abort_commit<B: AudioBackend>(
    state: &mut SessionState<B>,
    revision: TransportRevision,
) -> Result<(), SessionError> {
    let ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    let control = state
        .render_context_control
        .as_ref()
        .ok_or_else(|| SessionError::Graph("render context control is missing".into()))?;
    control.queue_abort(ctx, revision);
    let previous = state.transport.observed();
    let ledger = *state.transport.ledger();
    state.transport = SessionTransportState::Aborting {
        ledger,
        previous,
        revision,
        delivery: AbortDelivery::Pending,
    };
    deliver_abort(state)
}

fn deliver_abort<B: AudioBackend>(state: &mut SessionState<B>) -> Result<(), SessionError> {
    update_context(state)?;
    if let SessionTransportState::Aborting { delivery, .. } = &mut state.transport
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

pub(super) fn snapshot<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Result<SessionTransportSnapshot, SessionError> {
    refresh_observation(state)?
        .snapshot()
        .ok_or(SessionError::TransportNotProcessed)
}

pub(super) fn preparation_context<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Result<PreparationContext, SessionError> {
    let _ = refresh_observation(state)?;
    let commit = preparation_commit(&state.transport)?;
    let stream_info = state
        .ctx
        .as_ref()
        .and_then(FirewheelCtx::stream_info)
        .ok_or(SessionError::NoContext)?;
    Ok(PreparationContext::new(
        StreamShape {
            sample_rate: stream_info.sample_rate,
            max_block_frames: stream_info.max_block_frames,
        },
        commit.tempo(),
        commit.revision(),
        state.roster_revision,
    ))
}

pub(super) fn stream_shape<B: AudioBackend>(
    state: &SessionState<B>,
) -> Result<StreamShape, SessionError> {
    let stream_info = state
        .ctx
        .as_ref()
        .and_then(FirewheelCtx::stream_info)
        .ok_or(SessionError::NoContext)?;
    Ok(StreamShape {
        sample_rate: stream_info.sample_rate,
        max_block_frames: stream_info.max_block_frames,
    })
}

fn preparation_commit(
    transport: &SessionTransportState,
) -> Result<SessionTransportCommit, SessionError> {
    match transport {
        SessionTransportState::Stable { active, .. } => Ok(*active),
        SessionTransportState::Applying {
            next,
            previous: None,
            ..
        } => Ok(*next),
        SessionTransportState::Applying {
            next,
            previous: Some(_),
            ..
        } => Err(SessionError::TransportCommitPending {
            revision: next.revision(),
        }),
        SessionTransportState::Aborting {
            previous: Some(_),
            revision,
            ..
        } => Err(SessionError::TransportCommitPending {
            revision: *revision,
        }),
        SessionTransportState::Unconfigured { .. }
        | SessionTransportState::Aborting { previous: None, .. } => {
            Err(SessionError::TransportNotConfigured)
        }
    }
}

fn refresh_observation<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Result<TransportObservation, SessionError> {
    let observation = state
        .render_context_control
        .as_mut()
        .ok_or_else(|| SessionError::Graph("render context control is missing".into()))?
        .observation();
    if let Some(completion) = observation.completion() {
        apply_completion(state, completion);
    }
    if matches!(
        state.transport,
        SessionTransportState::Aborting {
            delivery: AbortDelivery::Pending,
            ..
        }
    ) {
        deliver_abort(state)?;
    }
    if let Some(revision) = state.transport.ledger_mut().rejected.take() {
        return Err(SessionError::TransportCommitRejected { revision });
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
    let previous = state.transport.observed();
    let current = std::mem::take(&mut state.transport);
    let (mut ledger, active, committed, rejected) = match (completion, current) {
        (
            TransportCommitResult::Applied(_),
            SessionTransportState::Applying {
                ledger,
                next,
                previous: _,
            },
        ) if next.revision() == revision => (ledger, Some(next), Some(next), false),
        (
            TransportCommitResult::Aborted(_),
            SessionTransportState::Aborting {
                delivery: AbortDelivery::Sent,
                ledger,
                previous,
                revision: pending_revision,
            },
        ) if pending_revision == revision => (ledger, previous, None, false),
        (_, current) => (*current.ledger(), current.observed(), None, true),
    };
    ledger.completed = Some(revision);
    if rejected {
        ledger.rejected = Some(revision);
    }
    state.transport = active.map_or(SessionTransportState::Unconfigured { ledger }, |active| {
        SessionTransportState::Stable { active, ledger }
    });
    if let Some(next) = committed {
        publish_transport_commit(state, previous, next);
    } else if rejected {
        publish_transport_event(
            state,
            &TransportEvent::Failed {
                revision: Some(revision.get()),
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
) -> [Option<TransportEvent>; 2] {
    let revision = next.revision().get();
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
    [tempo, play_state]
}

fn publish_transport_event<B: AudioBackend>(state: &SessionState<B>, event: &TransportEvent) {
    for player in &state.players {
        player.bus.publish(event.clone());
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::session::state::TransportLedger;

    const fn revision(value: u64) -> TransportRevision {
        TransportRevision::new_for_test(value)
    }

    fn commit(tempo: f64, revision: u64) -> SessionTransportCommit {
        SessionTransportCommit::new(
            Tempo::new(tempo).expect("invariant: test tempo is valid"),
            true,
            self::revision(revision),
        )
    }

    #[kithara::test]
    fn preparation_uses_initial_accepted_commit_before_first_render() {
        let transport = SessionTransportState::Applying {
            ledger: TransportLedger::default(),
            next: commit(120.0, 1),
            previous: None,
        };

        assert_eq!(
            preparation_commit(&transport).expect("initial commit is preparable"),
            commit(120.0, 1)
        );
    }

    #[kithara::test]
    fn preparation_uses_observed_active_commit() {
        let active = commit(120.0, 1);
        let transport = SessionTransportState::Stable {
            active,
            ledger: TransportLedger::default(),
        };

        assert_eq!(
            preparation_commit(&transport).expect("active commit is preparable"),
            active
        );
    }

    #[kithara::test]
    fn preparation_rejects_while_a_future_revision_is_pending() {
        let transport = SessionTransportState::Applying {
            ledger: TransportLedger::default(),
            next: commit(90.0, 2),
            previous: Some(commit(120.0, 1)),
        };

        assert!(matches!(
            preparation_commit(&transport),
            Err(SessionError::TransportCommitPending { revision: pending })
                if pending == revision(2)
        ));
    }

    #[kithara::test]
    fn preparation_requires_a_configured_transport() {
        assert!(matches!(
            preparation_commit(&SessionTransportState::default()),
            Err(SessionError::TransportNotConfigured)
        ));
    }
}
