use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_effects::LimiterConfig;
use kithara_platform::{
    sync::{Arc, mpsc, mpsc::TryRecvError},
    time::Duration,
};
use kithara_play::PlayError;
use kithara_sync::GroupState;
use kithara_worker::{Dispatcher, Task, TaskConfig, TaskHandle, TickResult};
use thiserror::Error;
use tracing::warn;

use super::{
    super::{
        dispatch::{pump_before_work, run_host_cmd},
        protocol::{Cmd, HostCmd, HostCmdMsg, HostReply, Reply, SessionError},
        state::{RootView, SessionState, ensure_ctx},
    },
    OfflineSessionClient,
    backend::{BackendConfig, OfflineStream},
};
use crate::PlayerMember;

pub(super) const CHANNELS: usize = 2;

pub(super) enum OfflineMsg<S> {
    Host(HostCmdMsg<S>),
    Position {
        reply_tx: mpsc::Sender<u64>,
    },
    Render {
        position: u64,
        frames: u32,
        reply_tx: mpsc::Sender<Result<SampleBuffer, OfflineSessionError>>,
    },
}

struct OfflineSessionTask<S> {
    max_block_frames: NonZeroU32,
    cmd_rx: Option<mpsc::Receiver<OfflineMsg<S>>>,
    state: Option<SessionState<OfflineStream, S>>,
    pools: PoolRegion<S>,
    position: u64,
}

pub(crate) struct OfflineTaskConfig<S> {
    pub(crate) declared_latency: Duration,
    pub(crate) limiter: LimiterConfig,
    pub(crate) declick_frames: NonZeroU32,
    pub(crate) max_block_frames: NonZeroU32,
    pub(crate) sample_rate: NonZeroU32,
    pub(crate) pools: PoolRegion<S>,
}

impl<S> OfflineSessionTask<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn render(&mut self, position: u64, frames: u32) -> Result<SampleBuffer, OfflineSessionError> {
        if position != self.position {
            return Err(OfflineSessionError::CursorChanged {
                expected: position,
                actual: self.position,
            });
        }
        if frames == 0 || frames > self.max_block_frames.get() {
            return Err(OfflineSessionError::InvalidBlockFrames {
                requested: frames,
                maximum: self.max_block_frames.get(),
            });
        }
        let next_position = self
            .position
            .checked_add(u64::from(frames))
            .ok_or(OfflineSessionError::TimelineOverflow)?;
        let state = self
            .state
            .as_mut()
            .ok_or(OfflineSessionError::SessionGone)?;
        let output = render_block(state, frames, self.position, &self.pools)?;
        // The callback has consumed this block even if its owner receipt
        // cannot be recorded. Never offer the same PCM range a second time.
        self.position = next_position;
        pump_before_work(state).map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
        Ok(output)
    }

    fn tick_host(&mut self, message: HostCmdMsg<S>) -> TickResult {
        let HostCmdMsg { cmd, reply_tx } = message;
        if matches!(&cmd, HostCmd::Shutdown) {
            drop(self.cmd_rx.take());
            self.state.take();
            if reply_tx.send(HostReply::Ok).is_err() {
                warn!("offline Host shutdown reply receiver dropped");
            }
            return TickResult::Done;
        }
        let retirement_retry = match &cmd {
            HostCmd::Play(Cmd::StopPlayer { player_id }) => Some(Cmd::StopPlayer {
                player_id: *player_id,
            }),
            HostCmd::Play(Cmd::ReleaseSlot { player_id, slot }) => Some(Cmd::ReleaseSlot {
                player_id: *player_id,
                slot: *slot,
            }),
            HostCmd::Play(Cmd::UnregisterPlayer { player_id }) => Some(Cmd::UnregisterPlayer {
                player_id: *player_id,
            }),
            _ => None,
        };
        let mut reply = self.state.as_mut().map_or_else(
            || {
                HostReply::Err(PlayError::SessionGone {
                    reason: "offline session state is unavailable",
                })
            },
            |state| run_host_cmd(state, cmd),
        );
        if matches!(
            reply,
            HostReply::Play(Reply::Err(SessionError::CallbackQuiescencePending))
        ) && let Some(retry) = retirement_retry
            && let Some(state) = self.state.as_mut()
        {
            reply = match state.stream.as_mut().map(OfflineStream::poll_control) {
                Some(Ok(())) => run_host_cmd(state, HostCmd::Play(retry)),
                Some(Err(error)) => {
                    HostReply::Play(Reply::Err(SessionError::Graph(error.to_string())))
                }
                None => HostReply::Play(Reply::Err(SessionError::NoContext)),
            };
        }
        if reply_tx.send(reply).is_err() {
            warn!("offline Host command reply receiver dropped");
        }
        TickResult::Progress
    }

    fn tick_message(&mut self, message: OfflineMsg<S>) -> TickResult {
        match message {
            OfflineMsg::Host(message) => self.tick_host(message),
            OfflineMsg::Position { reply_tx } => self.tick_position(&reply_tx),
            OfflineMsg::Render {
                position,
                frames,
                reply_tx,
            } => self.tick_render(position, frames, &reply_tx),
        }
    }

    fn tick_position(&self, reply_tx: &mpsc::Sender<u64>) -> TickResult {
        if reply_tx.send(self.position).is_err() {
            warn!("offline position reply receiver dropped");
        }
        TickResult::Progress
    }

    fn tick_render(
        &mut self,
        position: u64,
        frames: u32,
        reply_tx: &mpsc::Sender<Result<SampleBuffer, OfflineSessionError>>,
    ) -> TickResult {
        let reply = self.render(position, frames);
        if reply_tx.send(reply).is_err() {
            warn!("offline render reply receiver dropped");
        }
        TickResult::Progress
    }
}

impl<S> Task for OfflineSessionTask<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn on_cancel(&mut self) {
        self.state.take();
    }

    fn tick(&mut self) -> TickResult {
        let Some(cmd_rx) = self.cmd_rx.as_ref() else {
            return TickResult::Done;
        };
        match cmd_rx.try_recv() {
            Ok(message) => self.tick_message(message),
            Err(TryRecvError::Disconnected) => TickResult::Done,
            Err(TryRecvError::Empty) => TickResult::Waiting,
            #[cfg(target_arch = "wasm32")]
            Err(_) => TickResult::Waiting,
        }
    }
}

pub(crate) fn spawn<S>(
    dispatcher: &Dispatcher,
    task_config: TaskConfig,
    root: GroupState<PlayerMember>,
    root_view: RootView,
    config: OfflineTaskConfig<S>,
) -> Result<(Arc<OfflineSessionClient<S>>, TaskHandle), PlayError>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let OfflineTaskConfig {
        pools,
        sample_rate,
        max_block_frames,
        declick_frames,
        declared_latency,
        limiter,
    } = config;
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let pending = dispatcher.reserve(task_config).map_err(|error| {
        PlayError::Internal(format!("offline session task reservation: {error}"))
    })?;
    let control = pending.context().control();
    let client = Arc::new(OfflineSessionClient::new(
        cmd_tx,
        control,
        root_view.clone(),
    ));
    let task = pending
        .start_local(move |_| {
            let start_stream = move |ctx: &mut firewheel::FirewheelContext, rate: u32| {
                let rate = NonZeroU32::new(rate)
                    .ok_or_else(|| "offline sample rate must be non-zero".to_owned())?;
                let config = BackendConfig::builder()
                    .block_frames(max_block_frames)
                    .declared_latency(declared_latency)
                    .sample_rate(rate)
                    .build();
                OfflineStream::start(ctx, config).map_err(|error| error.to_string())
            };
            OfflineSessionTask {
                cmd_rx: Some(cmd_rx),
                max_block_frames,
                pools,
                position: 0,
                state: Some(SessionState::new(
                    root,
                    root_view,
                    sample_rate,
                    Some(max_block_frames),
                    Some(declick_frames),
                    limiter,
                    start_stream,
                )),
            }
        })
        .map_err(|error| PlayError::Internal(format!("offline session task start: {error}")))?;
    Ok((client, task))
}

fn render_block<S>(
    state: &mut SessionState<OfflineStream, S>,
    frames: u32,
    position: u64,
    pools: &PoolRegion<S>,
) -> Result<SampleBuffer, OfflineSessionError>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    if state.ctx.is_none() {
        ensure_ctx(state, state.sample_rate_hint)
            .map_err(|error| OfflineSessionError::Graph(error.to_string()))?;
    }
    let total_samples = usize::try_from(frames)
        .map_err(|_| OfflineSessionError::SampleCountOverflow)?
        .checked_mul(CHANNELS)
        .ok_or(OfflineSessionError::SampleCountOverflow)?;
    let mut output = pools
        .get_with_len::<f32>(total_samples)
        .map_err(OfflineSessionError::Pool)?;
    state
        .ctx
        .as_mut()
        .ok_or(OfflineSessionError::GraphUnavailable)?
        .update()
        .map_err(|error| OfflineSessionError::Graph(format!("{error:?}")))?;
    state
        .stream
        .as_mut()
        .ok_or(OfflineSessionError::BackendUnavailable)?
        .render(
            position,
            usize::try_from(frames).map_err(|_| OfflineSessionError::TimelineOverflow)?,
            &mut output,
        )?;
    Ok(output)
}

#[derive(Debug, Error)]
pub(crate) enum OfflineSessionError {
    #[error("offline backend is unavailable")]
    BackendUnavailable,
    #[error("offline channel count cannot be represented")]
    ChannelCountOverflow,
    #[error("offline render expected cursor {expected}, but the session is at {actual}")]
    CursorChanged { expected: u64, actual: u64 },
    #[error("offline graph failed: {0}")]
    Graph(String),
    #[error("offline graph has not started")]
    GraphUnavailable,
    #[error("offline block requests {requested} frames, maximum is {maximum}")]
    InvalidBlockFrames { requested: u32, maximum: u32 },
    #[error("offline output pool failed: {0}")]
    Pool(kithara_bufpool::PoolError),
    #[error("offline sample count overflow")]
    SampleCountOverflow,
    #[error("offline session is gone")]
    SessionGone,
    #[error("offline timeline overflow")]
    TimelineOverflow,
}
