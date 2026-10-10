use std::{marker::PhantomData, num::NonZeroU32};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_command::{Live, ScopedConfig, mailbox};
use kithara_config::Config;
use kithara_platform::{
    maybe_send::MaybeSend,
    sync::{Arc, mpsc, mpsc::TryRecvError},
    time::Duration,
};
use kithara_play::PlayError;
use kithara_worker::{Dispatcher, Task, TaskConfig, TickResult};
use thiserror::Error;

use super::{
    OfflineSessionClient, OfflineTaskHandle, OfflineTaskRoute,
    backend::{BackendConfig, OfflineStream},
};
use crate::{
    HostCore, HostOwner, HostSettings,
    rt::SessionOutput,
    session::{
        decks::{DeckInbox, DeckMsg},
        dispatch::OwnerPosts,
        protocol::HostMailbox,
        queue::HostProtocol,
        state::{HostRoot, RootView, SessionBufferConfig, SessionState, SessionStream},
    },
};
pub(crate) mod consts {
    pub(crate) const CHANNELS: usize = 2;
}

pub(in crate::session::offline) enum OfflineMsg {
    Posted,
    Deck(DeckMsg),
    Request(OfflineRequest),
    Shutdown(mpsc::Sender<()>),
}

pub(in crate::session::offline) enum OfflineRequest {
    Position(mpsc::Sender<u64>),
    Render {
        position: u64,
        frames: u32,
        answer: mpsc::Sender<Result<SampleBuffer, OfflineSessionError>>,
    },
}

struct OfflineSessionTask<S, O: HostOwner<S>> {
    cmd_rx: mpsc::Receiver<OfflineMsg>,
    mailbox: HostMailbox<O::Command>,
    owner: O,
    /// Dropped after the owner, acknowledging that retained controls are closed.
    shutdown_completion: Option<mpsc::Sender<()>>,
    posts: OwnerPosts,
    position: u64,
    max_block_frames: NonZeroU32,
    pools: PoolRegion<S>,
    marker: PhantomData<fn() -> S>,
}
#[derive(Config)]
#[config(construction)]
pub(crate) struct OfflineTaskConfig<S> {
    #[config(skip = "applied to the offline backend")]
    pub(crate) declared_latency: Duration,
    #[config(skip = "transferred to session output")]
    pub(crate) output: SessionOutput,
    #[config(skip = "transferred to session state")]
    pub(crate) settings: Live<HostSettings, HostProtocol>,
    #[config(skip = "transferred to session state")]
    pub(crate) channel_config: ScopedConfig,
    #[config(skip = "transferred to session state")]
    pub(crate) declick_frames: NonZeroU32,
    #[config(skip = "transferred to the offline task")]
    pub(crate) max_block_frames: NonZeroU32,
    #[config(skip = "transferred to the offline task")]
    pub(crate) pools: PoolRegion<S>,
}

impl<S, O: HostOwner<S>> OfflineSessionTask<S, O>
where
    S: HasPool<f32>,
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
        let next = position
            .checked_add(u64::from(frames))
            .ok_or(OfflineSessionError::TimelineOverflow)?;
        let frames =
            usize::try_from(frames).map_err(|_| OfflineSessionError::SampleCountOverflow)?;
        let samples = frames
            .checked_mul(consts::CHANNELS)
            .ok_or(OfflineSessionError::SampleCountOverflow)?;
        let mut output = self.pools.get::<f32>();
        output
            .ensure_len(samples)
            .map_err(OfflineSessionError::Pool)?;
        self.owner
            .render_offline(position, frames, &mut output)
            .map_err(OfflineSessionError::Owner)?;
        self.position = next;
        Ok(output)
    }
}

type StartedOfflineTask<C> = (Arc<OfflineSessionClient<C>>, OfflineTaskHandle);

pub(crate) fn spawn<S, O>(
    dispatcher: &Dispatcher,
    task_config: TaskConfig,
    root: HostRoot,
    root_view: RootView,
    config: OfflineTaskConfig<S>,
    layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
) -> Result<StartedOfflineTask<O::Command>, PlayError>
where
    S: HasPool<f32> + Send + Sync + 'static,
    O: HostOwner<S>,
{
    let OfflineTaskConfig {
        pools,
        max_block_frames,
        declick_frames,
        declared_latency,
        output,
        settings,
        channel_config,
    } = config;
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (postbox, mailbox) = mailbox();
    let pending = dispatcher.reserve(task_config).map_err(|error| {
        PlayError::Internal(format!("offline session task reservation: {error}"))
    })?;
    let route = OfflineTaskRoute::new(&pending);
    let client = Arc::new(OfflineSessionClient::new(postbox, cmd_tx, route.clone()));
    let inbox: Arc<dyn DeckInbox> = client.clone();
    let factory = move |_| {
        let start = move |ctx: &mut firewheel::FirewheelContext, rate: u32| {
            let rate = NonZeroU32::new(rate)
                .ok_or_else(|| "offline sample rate must be non-zero".to_owned())?;
            let backend = BackendConfig::builder()
                .block_frames(max_block_frames)
                .declared_latency(declared_latency)
                .sample_rate(rate)
                .build();
            OfflineStream::start(ctx, backend)
                .map(|stream| SessionStream::Offline(Box::new(stream)))
                .map_err(|error| error.to_string())
        };
        let state = SessionState::new(
            root,
            root_view,
            SessionBufferConfig {
                max_block_frames: Some(max_block_frames),
                declick_frames: Some(declick_frames),
            },
            output,
            settings,
            channel_config,
            start,
        );
        OfflineSessionTask {
            cmd_rx,
            mailbox,
            owner: layer(HostCore::new(state, inbox)),
            shutdown_completion: None,
            posts: OwnerPosts::new(),
            position: 0,
            max_block_frames,
            pools,
            marker: PhantomData,
        }
    };
    #[cfg(target_arch = "wasm32")]
    let task = route.start(pending, factory);
    #[cfg(not(target_arch = "wasm32"))]
    let task = OfflineTaskRoute::start(pending, factory);
    let task =
        task.map_err(|error| PlayError::Internal(format!("offline session task start: {error}")))?;
    Ok((client, task))
}
#[derive(Debug, Error)]
pub(crate) enum OfflineSessionError {
    #[error("offline channel count cannot be represented")]
    ChannelCountOverflow,
    #[error("offline render expected cursor {expected}, but the session is at {actual}")]
    CursorChanged { expected: u64, actual: u64 },
    #[error("offline graph failed: {0}")]
    Graph(String),
    #[error("offline block requests {requested} frames, maximum is {maximum}")]
    InvalidBlockFrames { requested: u32, maximum: u32 },
    #[error("offline owner failed: {0}")]
    Owner(PlayError),
    #[error("offline output pool failed: {0}")]
    Pool(kithara_bufpool::PoolError),
    #[error("offline sample count overflow")]
    SampleCountOverflow,
    #[error("offline session is gone")]
    SessionGone,
    #[error("offline timeline overflow")]
    TimelineOverflow,
}

impl<S, O: HostOwner<S>> Task for OfflineSessionTask<S, O>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn tick(&mut self) -> TickResult {
        self.owner.begin_pass();
        let mut progress = false;
        let mut requests: Vec<OfflineRequest> = Vec::new();
        let mut stopped = false;
        loop {
            match self.cmd_rx.try_recv() {
                Ok(OfflineMsg::Posted) => {
                    self.posts.drain(&mut self.owner, &mut self.mailbox);
                    progress = true;
                }
                Ok(OfflineMsg::Deck(message)) => {
                    message.run(&mut self.owner);
                    progress = true;
                }
                Ok(OfflineMsg::Request(request)) => {
                    requests.push(request);
                    progress = true;
                }
                Ok(OfflineMsg::Shutdown(completion)) => {
                    self.shutdown_completion = Some(completion);
                    stopped = true;
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    stopped = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                #[cfg(target_arch = "wasm32")]
                Err(_) => break,
            }
        }
        self.posts.drain(&mut self.owner, &mut self.mailbox);
        let mut published = false;
        for request in requests {
            match request {
                OfflineRequest::Position(answer) => {
                    let _ = answer.send(self.position);
                }
                OfflineRequest::Render {
                    position,
                    frames,
                    answer,
                } => {
                    let prepared = self.owner.prepare_offline();
                    self.owner.begin_pass();
                    self.posts.pass(&mut self.owner, true);
                    published = true;
                    let result = prepared
                        .map_err(OfflineSessionError::Owner)
                        .and_then(|()| self.render(position, frames));
                    if result.is_ok() {
                        self.owner.begin_pass();
                        self.posts.pass(&mut self.owner, false);
                    }
                    drop(answer.send(result));
                }
            }
        }
        if !published {
            self.posts.pass(&mut self.owner, false);
        }
        if stopped {
            TickResult::Done
        } else if progress {
            TickResult::Progress
        } else {
            TickResult::Waiting
        }
    }
}
