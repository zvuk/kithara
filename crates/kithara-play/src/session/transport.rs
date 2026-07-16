use std::num::NonZeroU32;

use firewheel::{backend::AudioBackend, error::UpdateError};

use super::{
    protocol::SessionError,
    state::{PendingTransportCommit, SessionState, TransportCommitPhase},
};
use crate::{
    api::{SessionTransportSnapshot, Tempo},
    rt::{
        RenderFrame, SessionTransportCommit, TransportCommitResult, TransportCommitStamp,
        TransportObservation,
    },
};

pub(super) fn set_tempo<B: AudioBackend>(
    state: &mut SessionState<B>,
    tempo: Tempo,
) -> Result<(), SessionError> {
    let _ = refresh_observation(state)?;
    if state.transport.accepted.map(SessionTransportCommit::tempo) == Some(tempo) {
        return Ok(());
    }
    if let Some(pending) = state.transport.pending {
        return Err(SessionError::TransportCommitPending {
            revision: pending.revision,
        });
    }

    let revision = state
        .transport
        .last_revision
        .checked_add(1)
        .ok_or(SessionError::TransportRevisionExhausted)?;
    let (target_frame, sample_rate) = commit_boundary(state)?;
    let next = SessionTransportCommit::new(tempo, true, revision);
    let stamp =
        TransportCommitStamp::new(state.transport.observed, next, target_frame, sample_rate);

    queue_stamp(state, stamp)?;
    state.transport.last_revision = revision;
    if let Err(error) = update_context(state) {
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
}
