use std::{any::Any, num::NonZeroU32};

use firewheel::FirewheelCtx;
use kithara::{
    audio::ConsumerWakeMode,
    bufpool::SamplePool,
    events::EventBus,
    host::testing::GraphSession,
    platform::{
        sync::{Mutex, mpsc},
        thread::{JoinHandle, spawn_named},
    },
    play::{Cmd, PlayError, Reply, SessionDispatcher, SessionError},
    warp::{BeatGridId, BeatGridIdAllocationError},
};

use super::{
    MasterRing, RingBackend, RingBackendConfig, RingBackendProbe, RingLayout, RingReader,
    RingRenderError,
};

type RingSetup = Box<
    dyn FnOnce(&mut FirewheelCtx<RingBackend>) -> Result<(), RingSessionError> + Send + 'static,
>;

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct ManualRingConfig {
    pub session_rate: NonZeroU32,
    pub block_frames: u32,
    pub capacity_blocks: usize,
    pub layout: RingLayout,
}

impl ManualRingConfig {
    #[must_use]
    pub const fn new(session_rate: NonZeroU32, block_frames: u32, capacity_blocks: usize) -> Self {
        Self {
            session_rate,
            block_frames,
            capacity_blocks,
            layout: RingLayout::Stereo,
        }
    }
}

impl Default for ManualRingConfig {
    fn default() -> Self {
        Self::new(
            NonZeroU32::new(44_100)
                .expect("invariant: default manual ring session rate is non-zero"),
            512,
            8,
        )
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RingSessionError {
    #[error(transparent)]
    GridId(#[from] BeatGridIdAllocationError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Render(#[from] RingRenderError),
    #[error("ring fixture setup failed: {0}")]
    Setup(String),
    #[error("ring context update failed: {0}")]
    Update(String),
    #[error("ring session context is not started")]
    NotStarted,
    #[error("ring session protocol violation: {0}")]
    Protocol(&'static str),
    #[error("ring session clock became negative: {0}")]
    NegativeClock(i64),
    #[error("ring session worker stopped")]
    WorkerStopped,
    #[error("ring session worker disappeared")]
    WorkerGone,
    #[error("ring session worker panicked: {message}")]
    WorkerPanicked { message: String },
}

enum RingMsg {
    Cmd {
        cmd: Cmd,
        reply_tx: mpsc::Sender<Reply>,
    },
    Credit {
        blocks: usize,
        reply_tx: mpsc::Sender<CreditReply>,
    },
    Shutdown,
}

struct CreditReply {
    error: Option<RingSessionError>,
    snapshot: Option<RingSnapshot>,
}

#[derive(Clone, Copy, Debug, Default)]
struct RingSnapshot {
    clock_samples: u64,
    committed_frames: u64,
}

pub struct ManualRingSession {
    cmd_tx: Mutex<Option<mpsc::Sender<RingMsg>>>,
    credit_gate: Mutex<()>,
    lifecycle_gate: Mutex<()>,
    probe: RingBackendProbe,
    reader: Mutex<RingReader>,
    snapshot: Mutex<RingSnapshot>,
    terminal_error: Mutex<Option<RingSessionError>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ManualRingSession {
    pub fn start(config: ManualRingConfig) -> Result<Self, RingSessionError> {
        Self::start_with(config, |_| Ok(()))
    }

    /// `no_block`: startup waits for the dedicated ring-session worker to finish arming.
    #[kithara::allow_block]
    pub fn start_with<F>(config: ManualRingConfig, setup: F) -> Result<Self, RingSessionError>
    where
        F: FnOnce(&mut FirewheelCtx<RingBackend>) -> Result<(), RingSessionError> + Send + 'static,
    {
        let (writer, reader) = MasterRing::open(config.block_frames, config.capacity_blocks);
        let probe = RingBackendProbe::default();
        let backend_config = RingBackendConfig::new(config.session_rate, config.layout, writer)
            .with_probe(probe.clone());
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let starter_probe = probe.clone();
        let worker = spawn_named("kithara-engine-manual-ring", move || {
            ring_session_thread(
                &cmd_rx,
                &ready_tx,
                backend_config,
                config.session_rate,
                starter_probe,
                Box::new(setup),
            );
        });
        let session = Self {
            cmd_tx: Mutex::new(Some(cmd_tx)),
            credit_gate: Mutex::new(()),
            lifecycle_gate: Mutex::new(()),
            probe,
            reader: Mutex::new(reader),
            snapshot: Mutex::new(RingSnapshot::default()),
            terminal_error: Mutex::new(None),
            worker: Mutex::new(Some(worker)),
        };
        match ready_rx.recv() {
            Ok(Ok(snapshot)) => {
                *session.snapshot.lock() = snapshot;
                Ok(session)
            }
            Ok(Err(error)) => {
                session.cmd_tx.lock().take();
                let _ = session.join_worker();
                Err(error)
            }
            Err(_) => session.worker_failure(),
        }
    }

    /// `no_block`: sync command-reply bridge to the dedicated ring-session worker.
    #[kithara::allow_block]
    pub fn exec(&self, cmd: Cmd) -> Result<Reply, RingSessionError> {
        self.ensure_available()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        let Some(cmd_tx) = self.cmd_tx.lock().clone() else {
            return self.worker_failure();
        };
        let sent = cmd_tx.send(RingMsg::Cmd { cmd, reply_tx });
        if sent.is_err() {
            return self.worker_failure();
        }
        match reply_rx.recv() {
            Ok(reply) => Ok(reply),
            Err(_) => self.worker_failure(),
        }
    }

    /// `no_block`: sync credit-reply bridge to the dedicated ring-session worker.
    #[kithara::allow_block]
    pub fn credit(&self, blocks: usize) -> Result<(), RingSessionError> {
        let _credit = self.credit_gate.lock();
        self.ensure_available()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        let Some(cmd_tx) = self.cmd_tx.lock().clone() else {
            return self.worker_failure();
        };
        let sent = cmd_tx.send(RingMsg::Credit { blocks, reply_tx });
        if sent.is_err() {
            return self.worker_failure();
        }
        let Ok(reply) = reply_rx.recv() else {
            return self.worker_failure();
        };
        if let Some(snapshot) = reply.snapshot {
            *self.snapshot.lock() = snapshot;
        }
        match reply.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn drain(&self, frames: usize) -> Result<Vec<f32>, RingSessionError> {
        self.ensure_available()?;
        Ok(self.reader.lock().drain(frames))
    }

    pub fn committed_frames(&self) -> Result<u64, RingSessionError> {
        self.ensure_available()?;
        Ok(self.snapshot.lock().committed_frames)
    }

    pub fn clock_samples(&self) -> Result<u64, RingSessionError> {
        self.ensure_available()?;
        Ok(self.snapshot.lock().clock_samples)
    }

    pub fn start_count(&self) -> Result<usize, RingSessionError> {
        self.ensure_available()?;
        Ok(self.probe.start_count())
    }

    pub fn pre_arm_error(&self) -> Result<Option<RingRenderError>, RingSessionError> {
        self.ensure_available()?;
        Ok(self.probe.pre_arm_error())
    }

    /// `no_block`: explicit shutdown joins the dedicated ring-session worker.
    #[kithara::allow_block]
    pub fn shutdown(&self) -> Result<(), RingSessionError> {
        let _lifecycle = self.lifecycle_gate.lock();
        if let Some(error) = self.terminal_error.lock().clone() {
            return match error {
                RingSessionError::WorkerStopped => Ok(()),
                other => Err(other),
            };
        }
        if let Some(tx) = self.cmd_tx.lock().take() {
            let _ = tx.send(RingMsg::Shutdown);
        }
        match self.join_worker() {
            Ok(()) => {
                *self.terminal_error.lock() = Some(RingSessionError::WorkerStopped);
                Ok(())
            }
            Err(error) => {
                *self.terminal_error.lock() = Some(error.clone());
                Err(error)
            }
        }
    }

    fn ensure_available(&self) -> Result<(), RingSessionError> {
        if let Some(error) = self.terminal_error.lock().clone() {
            return Err(error);
        }
        let worker_finished = self
            .worker
            .lock()
            .as_ref()
            .is_some_and(JoinHandle::is_finished);
        if worker_finished {
            return self.worker_failure();
        }
        let worker_missing = self.worker.lock().is_none();
        let sender_missing = self.cmd_tx.lock().is_none();
        if worker_missing || sender_missing {
            return self.worker_failure();
        }
        Ok(())
    }

    fn join_worker(&self) -> Result<(), RingSessionError> {
        let Some(worker) = self.worker.lock().take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|payload| RingSessionError::WorkerPanicked {
                message: panic_message(payload.as_ref()),
            })
    }

    fn worker_failure<T>(&self) -> Result<T, RingSessionError> {
        let _lifecycle = self.lifecycle_gate.lock();
        if let Some(error) = self.terminal_error.lock().clone() {
            return Err(error);
        }
        self.cmd_tx.lock().take();
        let error = match self.join_worker() {
            Ok(()) => RingSessionError::WorkerGone,
            Err(error) => error,
        };
        *self.terminal_error.lock() = Some(error.clone());
        Err(error)
    }
}

impl SessionDispatcher for ManualRingSession {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        Self::exec(self, cmd).map_err(|error| PlayError::Internal(error.to_string()))
    }

    /// The ring backend drives the device callback's processor, so the
    /// consumers this session hosts are real-time consumers.
    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

impl Drop for ManualRingSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn ring_session_thread(
    cmd_rx: &mpsc::Receiver<RingMsg>,
    ready_tx: &mpsc::Sender<Result<RingSnapshot, RingSessionError>>,
    backend_config: RingBackendConfig,
    session_rate: NonZeroU32,
    probe: RingBackendProbe,
    setup: RingSetup,
) {
    let mut backend_config = Some(backend_config);
    let mut state = GraphSession::<RingBackend>::new(move |ctx, _sample_rate| {
        let config = backend_config
            .take()
            .ok_or_else(|| String::from("ring backend cannot be restarted"))?;
        ctx.start_stream(config)
            .map_err(|error| error.to_string())?;
        let backend = ctx
            .active_backend_mut()
            .ok_or_else(|| String::from("ring backend missing after stream start"))?;
        match backend.render_block(0) {
            Err(RingRenderError::NotArmed) => {
                probe.record_pre_arm_error(RingRenderError::NotArmed);
            }
            Err(error) => return Err(format!("unexpected pre-arm render result: {error}")),
            Ok(()) => return Err(String::from("pre-arm ring render was accepted")),
        }
        backend.arm();
        Ok(())
    });
    let ready = bootstrap(&mut state, session_rate, setup).and_then(|()| snapshot(&mut state));
    let is_ready = ready.is_ok();
    if ready_tx.send(ready).is_err() || !is_ready {
        return;
    }
    for message in cmd_rx.iter() {
        match message {
            RingMsg::Cmd { cmd, reply_tx } => {
                let _ = reply_tx.send(state.exec(cmd));
            }
            RingMsg::Credit { blocks, reply_tx } => {
                let _ = reply_tx.send(credit_blocks(&mut state, blocks));
            }
            RingMsg::Shutdown => return,
        }
    }
}

fn bootstrap(
    state: &mut GraphSession<RingBackend>,
    session_rate: NonZeroU32,
    setup: RingSetup,
) -> Result<(), RingSessionError> {
    let player_id = match state.exec(Cmd::RegisterPlayer {
        grid_id: BeatGridId::allocate().map_err(RingSessionError::GridId)?,
        bus: EventBus::default(),
        eq_layout: Vec::new(),
        sample_pool: SamplePool::default(),
        sample_rate: session_rate.get(),
    }) {
        Reply::PlayerRegistered(player_id) => player_id,
        Reply::Err(error) => return Err(error.into()),
        _ => return Err(RingSessionError::Protocol("register anchor player reply")),
    };
    match state.exec(Cmd::StartPlayer {
        master_volume: 1.0,
        player_id,
        sample_rate: session_rate.get(),
    }) {
        Reply::Ok => {}
        Reply::Err(error) => return Err(error.into()),
        _ => return Err(RingSessionError::Protocol("start anchor player reply")),
    }
    let ctx = state.ctx_mut().ok_or(RingSessionError::NotStarted)?;
    setup(ctx)
}

fn credit_blocks(state: &mut GraphSession<RingBackend>, blocks: usize) -> CreditReply {
    let mut latest = match snapshot(state) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return CreditReply {
                error: Some(error),
                snapshot: None,
            };
        }
    };
    for _ in 0..blocks {
        match render_transaction(state) {
            Ok(current) => latest = current,
            Err(error) => {
                return match snapshot(state) {
                    Ok(snapshot) => CreditReply {
                        error: Some(error),
                        snapshot: Some(snapshot),
                    },
                    Err(snapshot_error) => CreditReply {
                        error: Some(snapshot_error),
                        snapshot: None,
                    },
                };
            }
        }
    }
    CreditReply {
        error: None,
        snapshot: Some(latest),
    }
}

fn render_transaction(
    state: &mut GraphSession<RingBackend>,
) -> Result<RingSnapshot, RingSessionError> {
    let ctx = state.ctx_mut().ok_or(RingSessionError::NotStarted)?;
    ctx.update()
        .map_err(|error| RingSessionError::Update(format!("{error:?}")))?;
    let raw_clock = ctx.audio_clock().samples.0;
    let clock_samples =
        u64::try_from(raw_clock).map_err(|_| RingSessionError::NegativeClock(raw_clock))?;
    ctx.active_backend_mut()
        .ok_or(RingSessionError::NotStarted)?
        .render_block(clock_samples)?;
    snapshot_from_ctx(ctx)
}

fn snapshot(state: &mut GraphSession<RingBackend>) -> Result<RingSnapshot, RingSessionError> {
    let ctx = state.ctx_mut().ok_or(RingSessionError::NotStarted)?;
    snapshot_from_ctx(ctx)
}

fn snapshot_from_ctx(
    ctx: &mut FirewheelCtx<RingBackend>,
) -> Result<RingSnapshot, RingSessionError> {
    let committed_frames = ctx
        .active_backend_mut()
        .ok_or(RingSessionError::NotStarted)?
        .committed_frames();
    let raw_clock = ctx.audio_clock().samples.0;
    let clock_samples =
        u64::try_from(raw_clock).map_err(|_| RingSessionError::NegativeClock(raw_clock))?;
    Ok(RingSnapshot {
        clock_samples,
        committed_frames,
    })
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    String::from("non-string panic payload")
}
