use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use kithara_play::{GroupState, PlayError, player::PlayerMember};
use kithara_worker::{Dispatcher, Task, TaskConfig, TaskHandle, TickResult};
use thiserror::Error;
use tracing::warn;

use super::{
    dispatch::run_host_cmd,
    protocol::{HostCmd, HostCmdMsg, HostReply},
    state::{RootView, SessionState, ensure_ctx},
};

mod backend;
mod client;

use backend::{BackendConfig, OfflineBackend};
pub(crate) use client::OfflineSessionClient;

const CHANNELS: usize = 2;

enum OfflineMsg<S> {
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
    cmd_rx: Option<mpsc::Receiver<OfflineMsg<S>>>,
    max_block_frames: NonZeroU32,
    pools: PoolRegion<S>,
    position: u64,
    state: Option<SessionState<OfflineBackend, S>>,
    #[cfg(any(test, feature = "probe"))]
    pacing: Option<(Duration, Instant)>,
}

pub(crate) struct OfflineTaskConfig<S> {
    pub(crate) pools: PoolRegion<S>,
    pub(crate) sample_rate: NonZeroU32,
    pub(crate) max_block_frames: NonZeroU32,
    pub(crate) declick_frames: NonZeroU32,
    pub(crate) declared_latency: Duration,
    #[cfg(any(test, feature = "probe"))]
    pub(crate) pacing: Option<Duration>,
}

impl<S> OfflineSessionTask<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
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
        let reply = self.state.as_mut().map_or_else(
            || {
                HostReply::Err(PlayError::SessionGone {
                    reason: "offline session state is unavailable",
                })
            },
            |state| run_host_cmd(state, cmd),
        );
        if reply_tx.send(reply).is_err() {
            warn!("offline Host command reply receiver dropped");
        }
        TickResult::Progress
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
        let state = self
            .state
            .as_mut()
            .ok_or(OfflineSessionError::SessionGone)?;
        let output = render_block(state, frames, self.position, &self.pools)?;
        self.position = self
            .position
            .checked_add(u64::from(frames))
            .ok_or(OfflineSessionError::TimelineOverflow)?;
        Ok(output)
    }

    #[cfg(any(test, feature = "probe"))]
    fn tick_pacing(&mut self) -> TickResult {
        let Some((pacing, deadline)) = self.pacing else {
            return TickResult::Waiting;
        };
        if Instant::now() < deadline {
            return TickResult::Waiting;
        }
        self.pacing = Some((pacing, Instant::now() + pacing));
        match self.render(self.position, self.max_block_frames.get()) {
            Ok(_) => TickResult::Progress,
            Err(error) => {
                warn!(%error, "paced offline render failed");
                TickResult::Done
            }
        }
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
            Err(mpsc::TryRecvError::Disconnected) => TickResult::Done,
            Err(mpsc::TryRecvError::Empty) => {
                #[cfg(any(test, feature = "probe"))]
                {
                    self.tick_pacing()
                }
                #[cfg(not(any(test, feature = "probe")))]
                TickResult::Waiting
            }
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
    #[cfg(any(test, feature = "probe"))]
    if config.pacing.is_some_and(|interval| interval.is_zero()) {
        return Err(PlayError::SessionCategoryUnsupported {
            reason: "offline pacing interval must be non-zero".to_owned(),
        });
    }
    let OfflineTaskConfig {
        pools,
        sample_rate,
        max_block_frames,
        declick_frames,
        declared_latency,
        #[cfg(any(test, feature = "probe"))]
        pacing,
    } = config;
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let pending = dispatcher.reserve(task_config).map_err(|error| {
        PlayError::Internal(format!("offline session task reservation: {error}"))
    })?;
    let control = pending.context().control();
    let client = Arc::new(OfflineSessionClient::new(cmd_tx, control));
    let task = pending
        .start_local(move |_| {
            let start_stream = move |ctx: &mut firewheel::FirewheelCtx<OfflineBackend>,
                                     rate: u32| {
                let rate = NonZeroU32::new(rate)
                    .ok_or_else(|| "offline sample rate must be non-zero".to_owned())?;
                let config = BackendConfig::builder()
                    .block_frames(max_block_frames)
                    .declick_frames(declick_frames)
                    .declared_latency(declared_latency)
                    .sample_rate(rate)
                    .build();
                ctx.start_stream(config).map_err(|error| error.to_string())
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
                    start_stream,
                )),
                #[cfg(any(test, feature = "probe"))]
                pacing: pacing.map(|interval| (interval, Instant::now() + interval)),
            }
        })
        .map_err(|error| PlayError::Internal(format!("offline session task start: {error}")))?;
    Ok((client, task))
}

fn render_block<S>(
    state: &mut SessionState<OfflineBackend, S>,
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
    let ctx = state
        .ctx
        .as_mut()
        .ok_or(OfflineSessionError::GraphUnavailable)?;
    ctx.update()
        .map_err(|error| OfflineSessionError::Graph(format!("{error:?}")))?;
    ctx.active_backend_mut()
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
    #[error("offline processor is unavailable")]
    ProcessorUnavailable,
    #[error("offline output pool failed: {0}")]
    Pool(kithara_bufpool::PoolError),
    #[error("offline sample count overflow")]
    SampleCountOverflow,
    #[error("offline session is gone")]
    SessionGone,
    #[error("offline timeline overflow")]
    TimelineOverflow,
}
