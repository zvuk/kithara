use std::num::NonZeroU32;

use firewheel::{FirewheelCtx, backend::AudioBackend, error::UpdateError};
use kithara_events::TransportEvent;

use super::{
    PlayerId,
    protocol::{BindingPreparation, SessionError},
    render::{
        RenderFrame, SessionTransportCommit, TransportCommitResult, TransportCommitStamp,
        TransportObservation,
    },
    state::{PendingTransportCommit, SessionState, SessionTransportState, TransportCommitPhase},
};
use crate::{
    api::{SessionBeat, SessionTransportSnapshot, Tempo},
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
    expected_revision: u64,
    expected_shape: StreamShape,
    player_ids: &[PlayerId],
) -> Result<(), SessionError> {
    let _ = validate_checked_transport(state, expected_revision, expected_shape, player_ids)?;
    set_tempo_after_refresh(state, tempo)
}

pub(super) fn seek_checked<B: AudioBackend>(
    state: &mut SessionState<B>,
    target: SessionBeat,
    expected_revision: u64,
    expected_shape: StreamShape,
    player_ids: &[PlayerId],
) -> Result<(), SessionError> {
    let snapshot =
        validate_checked_transport(state, expected_revision, expected_shape, player_ids)?;
    ensure_no_pending_commit(state)?;
    let revision = next_revision(state)?;
    let (target_frame, sample_rate) = commit_boundary(state)?;
    let next = SessionTransportCommit::new_at_beat(
        snapshot.tempo(),
        snapshot.is_playing(),
        revision,
        target,
    );
    let stamp = TransportCommitStamp::new_at_beat(
        state.transport.observed,
        next,
        target_frame,
        target,
        sample_rate,
    );
    schedule_commit(state, next, stamp)
}

fn validate_checked_transport<B: AudioBackend>(
    state: &mut SessionState<B>,
    expected_revision: u64,
    expected_shape: StreamShape,
    player_ids: &[PlayerId],
) -> Result<SessionTransportSnapshot, SessionError> {
    let observation = refresh_observation(state)?;
    let snapshot = observation
        .snapshot()
        .ok_or(SessionError::TransportNotProcessed)?;
    let actual_revision = snapshot.revision();
    if actual_revision != expected_revision {
        return Err(SessionError::TransportRevisionChanged {
            expected: expected_revision,
            actual: actual_revision,
        });
    }
    if stream_shape(state)? != expected_shape {
        return Err(SessionError::TransportStreamShapeChanged);
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
    if state.transport.accepted.map(SessionTransportCommit::tempo) == Some(tempo) {
        return Ok(());
    }
    ensure_no_pending_commit(state)?;
    let revision = next_revision(state)?;
    let (target_frame, sample_rate) = commit_boundary(state)?;
    let next = SessionTransportCommit::new(tempo, true, revision);
    let stamp =
        TransportCommitStamp::new(state.transport.observed, next, target_frame, sample_rate);

    schedule_commit(state, next, stamp)
}

fn ensure_no_pending_commit<B: AudioBackend>(state: &SessionState<B>) -> Result<(), SessionError> {
    if let Some(pending) = state.transport.pending {
        return Err(SessionError::TransportCommitPending {
            revision: pending.revision,
        });
    }
    Ok(())
}

fn next_revision<B: AudioBackend>(state: &SessionState<B>) -> Result<u64, SessionError> {
    state
        .transport
        .last_revision
        .checked_add(1)
        .ok_or(SessionError::TransportRevisionExhausted)
}

fn schedule_commit<B: AudioBackend>(
    state: &mut SessionState<B>,
    next: SessionTransportCommit,
    stamp: TransportCommitStamp,
) -> Result<(), SessionError> {
    let revision = next.revision();

    queue_stamp(state, stamp)?;
    state.transport.last_revision = revision;
    if let Err(error) = update_context(state) {
        publish_transport_event(
            state,
            &TransportEvent::Failed {
                revision: Some(revision),
                reason: error.to_string(),
            },
        );
        abort_commit(state, revision);
        return Err(error);
    }

    state.transport.accepted = Some(next);
    state.transport.pending = Some(PendingTransportCommit {
        phase: TransportCommitPhase::Applying,
        revision,
    });
    Ok(())
}

fn commit_boundary<B: AudioBackend>(
    state: &SessionState<B>,
) -> Result<(RenderFrame, NonZeroU32), SessionError> {
    let ctx = state.ctx.as_ref().ok_or(SessionError::NoContext)?;
    let stream_info = ctx.stream_info().ok_or(SessionError::NoContext)?;
    let lead_frames = state
        .transport
        .observed
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

fn abort_commit<B: AudioBackend>(state: &mut SessionState<B>, revision: u64) {
    state.transport.pending = Some(PendingTransportCommit {
        phase: TransportCommitPhase::Aborting,
        revision,
    });
    if let (Some(ctx), Some(control)) = (state.ctx.as_mut(), state.render_context_control.as_ref())
    {
        control.queue_abort(ctx, revision);
        let _ = ctx.update();
    }
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

pub(super) fn binding_preparation<B: AudioBackend>(
    state: &mut SessionState<B>,
) -> Result<BindingPreparation, SessionError> {
    let _ = refresh_observation(state)?;
    let commit = preparation_commit(&state.transport)?;
    let stream_info = state
        .ctx
        .as_ref()
        .and_then(FirewheelCtx::stream_info)
        .ok_or(SessionError::NoContext)?;
    Ok(BindingPreparation {
        revision: commit.revision(),
        shape: StreamShape {
            sample_rate: stream_info.sample_rate,
            max_block_frames: stream_info.max_block_frames,
        },
        tempo: commit.tempo(),
    })
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
    if let Some(pending) = transport.pending
        && transport.observed.is_some()
    {
        return Err(SessionError::TransportCommitPending {
            revision: pending.revision,
        });
    }
    match (transport.observed, transport.accepted) {
        (Some(commit), _) => Ok(commit),
        (None, Some(initial)) => Ok(initial),
        (None, None) => Err(SessionError::TransportNotConfigured),
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
    if let Some(revision) = state.transport.rejected.take() {
        return Err(SessionError::TransportCommitRejected { revision });
    }
    Ok(observation)
}

fn apply_completion<B: AudioBackend>(
    state: &mut SessionState<B>,
    completion: TransportCommitResult,
) {
    let revision = completion.revision();
    if revision <= state.transport.completed_revision {
        return;
    }
    let previous = state.transport.observed;
    let mut committed = None;
    let accepted = match (completion, state.transport.pending) {
        (
            TransportCommitResult::Applied(_),
            Some(PendingTransportCommit {
                phase: TransportCommitPhase::Applying,
                revision: pending_revision,
            }),
        ) if pending_revision == revision => {
            if let Some(accepted) = state
                .transport
                .accepted
                .filter(|commit| commit.revision() == revision)
            {
                state.transport.observed = Some(accepted);
                committed = Some(accepted);
                Some(accepted)
            } else {
                state.transport.rejected = Some(revision);
                state.transport.observed
            }
        }
        (
            TransportCommitResult::Aborted(_),
            Some(PendingTransportCommit {
                phase: TransportCommitPhase::Aborting,
                revision: pending_revision,
            }),
        ) if pending_revision == revision => state.transport.observed,
        _ => {
            state.transport.rejected = Some(revision);
            state.transport.observed
        }
    };
    state.transport.accepted = accepted;
    state.transport.completed_revision = revision;
    state.transport.pending = None;
    if let Some(next) = committed {
        publish_transport_commit(state, previous, next);
    } else if state.transport.rejected == Some(revision) {
        publish_transport_event(
            state,
            &TransportEvent::Failed {
                revision: Some(revision),
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
    let revision = next.revision();
    let tempo = previous
        .is_none_or(|commit| commit.tempo() != next.tempo())
        .then(|| TransportEvent::TempoCommitted {
            beats_per_minute: next.tempo().beats_per_minute(),
            revision,
        });
    let play_state = previous
        .is_none_or(|commit| commit.is_playing() != next.is_playing())
        .then(|| TransportEvent::PlayStateCommitted {
            playing: next.is_playing(),
            revision,
        });
    let seek = next
        .seek_target()
        .map(|target| TransportEvent::SeekCommitted {
            position_beats: target.get(),
            revision,
        });
    [tempo, play_state, seek]
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

    fn commit(tempo: f64, revision: u64) -> SessionTransportCommit {
        SessionTransportCommit::new(Tempo::new(tempo).expect("valid tempo"), true, revision)
    }

    #[kithara::test]
    fn preparation_uses_initial_accepted_commit_before_first_render() {
        let transport = SessionTransportState {
            accepted: Some(commit(120.0, 1)),
            ..SessionTransportState::default()
        };

        assert_eq!(
            preparation_commit(&transport).expect("initial commit is preparable"),
            commit(120.0, 1)
        );
    }

    #[kithara::test]
    fn preparation_uses_observed_active_commit() {
        let active = commit(120.0, 1);
        let transport = SessionTransportState {
            accepted: Some(active),
            observed: Some(active),
            ..SessionTransportState::default()
        };

        assert_eq!(
            preparation_commit(&transport).expect("active commit is preparable"),
            active
        );
    }

    #[kithara::test]
    fn preparation_rejects_while_a_future_revision_is_pending() {
        let transport = SessionTransportState {
            accepted: Some(commit(90.0, 2)),
            observed: Some(commit(120.0, 1)),
            pending: Some(PendingTransportCommit {
                phase: TransportCommitPhase::Applying,
                revision: 2,
            }),
            ..SessionTransportState::default()
        };

        assert!(matches!(
            preparation_commit(&transport),
            Err(SessionError::TransportCommitPending { revision: 2 })
        ));
    }

    #[kithara::test]
    fn preparation_requires_a_configured_transport() {
        assert!(matches!(
            preparation_commit(&SessionTransportState::default()),
            Err(SessionError::TransportNotConfigured)
        ));
    }

    #[kithara::test]
    fn seek_commit_uses_only_transport_seek_vocabulary() {
        let current = commit(120.0, 1);
        let target = SessionBeat::new(16.0).expect("finite target");
        let next =
            SessionTransportCommit::new_at_beat(current.tempo(), current.is_playing(), 2, target);

        assert_eq!(
            transport_events(Some(current), next),
            [
                None,
                None,
                Some(TransportEvent::SeekCommitted {
                    position_beats: 16.0,
                    revision: 2,
                }),
            ]
        );
    }
}
