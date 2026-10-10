use std::num::NonZeroU32;

use firewheel::{FirewheelContext, error::UpdateError};
use kithara_command::{Answer, Post, Seq};
use kithara_config::ConfigOwner;
use kithara_play::{
    HostedDeck, PlayError, RouteChangeReason, RouteDescription, SessionEvent, StreamShape,
};
use tracing::{debug, trace, warn};

use super::{
    protocol::{HostMailbox, SessionError, SessionSampleRate},
    state::SessionState,
    transport::{self, RouteRestartStatus},
};
use crate::{DeckId, HostOwner, HostSettled};

pub(crate) fn run_host_cmd<S, O: HostOwner<S>>(
    owner: &mut O,
    command: O::Command,
) -> Result<Option<Seq>, PlayError> {
    owner.apply(command)
}

/// Only membership release waits for an executor receipt.
#[derive(Default)]
pub(crate) struct OwnerPosts {
    closing: Vec<(DeckId, Answer<PlayError>)>,
}

impl OwnerPosts {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn drain<S, O: HostOwner<S>>(
        &mut self,
        owner: &mut O,
        mailbox: &mut HostMailbox<O::Command>,
    ) {
        let mut posts = Self::take_posts(mailbox).into_iter().peekable();
        while let Some(Post { command, answer }) = posts.next() {
            if O::is_next_tempo(&command)
                && posts
                    .peek()
                    .is_some_and(|post| O::is_next_tempo(&post.command))
            {
                answer.answer(Err(PlayError::Superseded));
                continue;
            }
            let releasing = O::release_id(&command);
            match run_host_cmd(owner, command) {
                Ok(_) if releasing.is_some() => {
                    if let Some(id) = releasing {
                        self.closing.push((id, answer));
                    }
                }
                outcome => answer.answer(outcome.map(|_| ())),
            }
        }
    }

    fn take_posts<C>(mailbox: &mut HostMailbox<C>) -> Vec<Post<C, PlayError>> {
        mailbox.drain().collect()
    }

    pub(crate) fn pass<S, O: HostOwner<S>>(&mut self, owner: &mut O, tick: bool) {
        if tick && owner.clock().is_some() {
            owner.each_deck(&mut |_, deck, out, pass| deck.tick(pass, out));
        }
        let settled = owner.pass();
        if !self.closing.is_empty() {
            self.settle(settled);
        }
    }

    fn settle(&mut self, settled: Vec<HostSettled>) {
        for settled in settled {
            match settled {
                HostSettled::Closed { deck } => {
                    let mut index = 0;
                    while index < self.closing.len() {
                        if self.closing[index].0 == deck {
                            let (_, answer) = self.closing.remove(index);
                            answer.answer(Ok(()));
                        } else {
                            index += 1;
                        }
                    }
                }
                HostSettled::Settings { .. } | HostSettled::Batch { .. } => {}
            }
        }
    }
}
fn measured_stream_shape<T, S>(state: &SessionState<T, S>) -> Option<StreamShape> {
    if state.stream_needs_restart {
        return None;
    }
    state
        .ctx
        .as_ref()
        .and_then(FirewheelContext::stream_info)
        .map(|info| StreamShape::new(info.max_block_frames, info.sample_rate))
}

pub(super) fn sample_rate<T, S>(state: &SessionState<T, S>) -> SessionSampleRate {
    let measured = measured_stream_shape(state).map(|shape| shape.sample_rate.get());
    SessionSampleRate::new(measured, state.settings.config().sample_rate().get())
}

pub(super) fn stream_shape<T, S>(state: &SessionState<T, S>) -> Option<StreamShape> {
    measured_stream_shape(state).or_else(|| {
        Some(StreamShape::new(
            state.requested_max_block_frames?,
            state.settings.config().sample_rate(),
        ))
    })
}

/// One pump of the session on its own interval: a deferred or dead stream
/// restarts, the graph updates, and the transport's commits and receipts
/// settle.
pub(crate) fn tick_session<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    if state.stream_needs_restart {
        if let Err(err) = restart_stream(state) {
            warn!(?err, "[KITHARA-ROUTE] deferred stream restart failed");
            return Err(SessionError::RestartFailed {
                reason: "deferred stream restart".into(),
                r#source: err.to_string(),
            });
        }
        if state.stream_needs_restart {
            return Ok(());
        }
    }

    let update = state.ctx.as_mut().map(FirewheelContext::update);
    if let Some(Err(err)) = update {
        return Err(update_failed(&err));
    }
    if stream_died(state) {
        return restart_dead_stream(state);
    }
    transport::observe_commits(state);
    Ok(())
}

fn update_failed(err: &UpdateError) -> SessionError {
    warn!(?err, "[KITHARA-ROUTE] firewheel update failed");
    SessionError::Graph(format!("{err:?}"))
}

/// An inactive context without a reserved stopped generation lost its stream.
/// Firewheel returns the processor when it stops instead of reporting an update error.
pub(super) fn stream_died<T, S>(state: &SessionState<T, S>) -> bool {
    state.reserved_session_grid.is_none()
        && !state.stream_needs_restart
        && state.ctx.as_ref().is_some_and(|ctx| !ctx.is_active())
}

fn restart_dead_stream<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    state.stream_needs_restart = true;
    state.publish_root();
    warn!("session stream stopped unexpectedly; restarting audio stream");
    trace!(
        sample_rate = state.settings.config().sample_rate().get(),
        "[KITHARA-ROUTE] firewheel context went inactive under a live stream"
    );
    restart_stream(state).map_err(|restart_err| SessionError::RestartFailed {
        reason: "audio stream stopped".to_owned(),
        r#source: restart_err.to_string(),
    })
}

pub(crate) fn invalidate_audio_route<T, S>(
    state: &mut SessionState<T, S>,
    reason: &str,
) -> Result<(), SessionError> {
    debug!(
        reason,
        ctx_ready = state.ctx.is_some(),
        stream_needs_restart = state.stream_needs_restart,
        "[KITHARA-ROUTE] audio route invalidated"
    );
    if state.ctx.is_none() {
        return Ok(());
    }
    state.stream_needs_restart = true;
    restart_stream(state).map_err(|err| SessionError::RestartFailed {
        reason: reason.to_owned(),
        r#source: err.to_string(),
    })?;
    for deck in &state.deck_nodes {
        if let Some(bus) = &deck.bus {
            bus.publish(SessionEvent::RouteChanged {
                reason: RouteChangeReason::Unknown,
                previous_route: RouteDescription::default(),
            });
        }
    }
    Ok(())
}

/// Restarts the output at the rate the settings ask for.
pub(super) fn restart_stream<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    if state.ctx.is_none() {
        return Err(SessionError::NoContext);
    }
    let sample_rate = state.settings.config().sample_rate().get();
    debug!(sample_rate, "[KITHARA-ROUTE] restarting firewheel stream");
    if transport::prepare_route_restart(state)? == RouteRestartStatus::Pending {
        trace!("[KITHARA-ROUTE] waiting for the previous stream processor to stop");
        return Ok(());
    }
    let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    let stream = (state.start_stream_fn)(fw_ctx, sample_rate).map_err(SessionError::StreamStart)?;
    state.stream = Some(stream);
    state.reserved_session_grid = None;
    state.stream_needs_restart = false;
    state.publish_root();
    trace_stream_info(state, "restart-stream");
    debug!(
        sample_rate,
        "[KITHARA-ROUTE] firewheel stream restart complete"
    );
    Ok(())
}

pub(super) fn trace_stream_info<T, S>(state: &SessionState<T, S>, context: &'static str) {
    if let Some(info) = state.ctx.as_ref().and_then(FirewheelContext::stream_info) {
        trace!(
            context,
            sample_rate = info.sample_rate.get(),
            prev_sample_rate = info.prev_sample_rate.get(),
            max_block_frames = info.max_block_frames.get(),
            out_channels = info.num_stream_out_channels,
            stream_needs_restart = state.stream_needs_restart,
            "[KITHARA-ROUTE] session stream-info"
        );
    } else {
        trace!(
            context,
            sample_rate = state.settings.config().sample_rate().get(),
            requested_max_block_frames = state.requested_max_block_frames.map(NonZeroU32::get),
            stream_needs_restart = state.stream_needs_restart,
            "[KITHARA-ROUTE] session stream-info unavailable"
        );
    }
}
