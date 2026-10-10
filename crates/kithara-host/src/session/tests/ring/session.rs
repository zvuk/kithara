use std::{any::Any, num::NonZeroU32, sync::OnceLock};

use firewheel::FirewheelContext;
use kithara_command::When;
use kithara_events::EventBus;
use kithara_platform::{
    sync::{Mutex, mpsc},
    thread::{JoinHandle, spawn_named},
};
use kithara_play::{PlayError, SessionError, SessionOutputView, SessionTransportSnapshot};
use kithara_signal::SessionFrame;
use kithara_test_utils::{bufpool::pools, kithara};
use kithara_warp::{BeatGridId, BeatGridIdAllocationError};

use super::{
    super::graph::GraphSession, MasterRing, RingBackend, RingBackendConfig, RingBackendProbe,
    RingLayout, RingReader, RingRenderError,
};
use crate::{HostSettingsChange, session::state::RootView};

type RingSetup =
    Box<dyn FnOnce(&mut FirewheelContext) -> Result<(), RingSessionError> + Send + 'static>;

#[kithara::test(native, tokio)]
async fn ring_binding_keeps_resource_events_off_the_render_thread() {
    let session = ManualRingSession::start(ManualRingConfig::new(
        kithara_play::mock::SAMPLE_RATE,
        128,
        4,
    ))
    .expect("offline session starts");
    let pools = pools();
    let dir = kithara_test_utils::TestTempDir::new();
    let prep = kithara_play::ResourcePrep::builder()
        .worker(kithara_play::PlayWorker::new(
            kithara_play::PlayWorkerConfig::builder(pools.clone()).build(),
        ))
        .build();
    kithara_play::mock::assert_prepared_render_off_bus(
        &prep,
        &kithara_play::mock::output(None).get(),
        &pools,
        &dir.path().join("session.wav"),
    )
    .await
    .expect("session-prepared lane renders off the bus");
    session.shutdown().expect("offline session stops");
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub(crate) struct ManualRingConfig {
    pub(crate) session_rate: NonZeroU32,
    pub(crate) layout: RingLayout,
    pub(crate) block_frames: u32,
    pub(crate) capacity_blocks: usize,
}

impl ManualRingConfig {
    #[must_use]
    pub(crate) const fn new(
        session_rate: NonZeroU32,
        block_frames: u32,
        capacity_blocks: usize,
    ) -> Self {
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
pub(crate) enum RingSessionError {
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
    Tick {
        reply_tx: mpsc::Sender<Result<(), SessionError>>,
    },
    Install {
        id: BeatGridId,
        bus: EventBus,
        reply_tx: mpsc::Sender<Result<(), PlayError>>,
    },
    Remove {
        id: BeatGridId,
        reply_tx: mpsc::Sender<Result<(), PlayError>>,
    },
    Configure {
        change: HostSettingsChange,
        at: When<SessionFrame>,
        reply_tx: mpsc::Sender<Result<(), PlayError>>,
    },
    Credit {
        blocks: usize,
        reply_tx: mpsc::Sender<CreditReply>,
    },
    Transport {
        reply_tx: mpsc::Sender<Option<SessionTransportSnapshot>>,
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

pub(crate) struct ManualRingSession {
    cmd_tx: Mutex<Option<mpsc::Sender<RingMsg>>>,
    credit_gate: Mutex<()>,
    lifecycle_gate: Mutex<()>,
    reader: Mutex<RingReader>,
    snapshot: Mutex<RingSnapshot>,
    terminal_error: Mutex<Option<RingSessionError>>,
    view: OnceLock<RootView>,
    worker: Mutex<Option<JoinHandle<()>>>,
    probe: RingBackendProbe,
}

impl ManualRingSession {
    pub(crate) fn clock_samples(&self) -> Result<u64, RingSessionError> {
        self.ensure_available()?;
        Ok(self.snapshot.lock().clock_samples)
    }

    pub(crate) fn committed_frames(&self) -> Result<u64, RingSessionError> {
        self.ensure_available()?;
        Ok(self.snapshot.lock().committed_frames)
    }

    /// `no_block`: sync credit-reply bridge to the dedicated ring-session worker.
    #[kithara::allow_block]
    pub(crate) fn credit(&self, blocks: usize) -> Result<(), RingSessionError> {
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
        reply.error.map_or(Ok(()), Err)
    }

    pub(crate) fn drain(&self, frames: usize) -> Result<Vec<f32>, RingSessionError> {
        self.ensure_available()?;
        Ok(self.reader.lock().drain(frames))
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

    /// Pumps the session once, as its owner does on its interval; call from a
    /// blocking control thread.
    pub(crate) fn tick(&self) -> Result<Result<(), SessionError>, RingSessionError> {
        self.ensure_available()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        let Some(cmd_tx) = self.cmd_tx.lock().clone() else {
            return self.worker_failure();
        };
        let sent = cmd_tx.send(RingMsg::Tick { reply_tx });
        if sent.is_err() {
            return self.worker_failure();
        }
        reply_rx.recv().map_or_else(|_| self.worker_failure(), Ok)
    }

    /// Runs `cmd` and waits for whether it applied; call from a blocking
    /// control thread.
    pub(crate) fn install(
        &self,
        id: BeatGridId,
        bus: EventBus,
    ) -> Result<Result<(), PlayError>, RingSessionError> {
        self.post(|reply_tx| RingMsg::Install { id, bus, reply_tx })
    }

    pub(crate) fn remove(&self, id: BeatGridId) -> Result<Result<(), PlayError>, RingSessionError> {
        self.post(|reply_tx| RingMsg::Remove { id, reply_tx })
    }

    pub(crate) fn configure(
        &self,
        change: HostSettingsChange,
        at: When<SessionFrame>,
    ) -> Result<Result<(), PlayError>, RingSessionError> {
        self.post(|reply_tx| RingMsg::Configure {
            change,
            at,
            reply_tx,
        })
    }

    fn post(
        &self,
        message: impl FnOnce(mpsc::Sender<Result<(), PlayError>>) -> RingMsg,
    ) -> Result<Result<(), PlayError>, RingSessionError> {
        self.ensure_available()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        let Some(cmd_tx) = self.cmd_tx.lock().clone() else {
            return self.worker_failure();
        };
        if cmd_tx.send(message(reply_tx)).is_err() {
            return self.worker_failure();
        }
        reply_rx.recv().map_or_else(|_| self.worker_failure(), Ok)
    }

    /// What the transport last committed; `None` while a route restart holds
    /// the session grid.
    pub(crate) fn transport(&self) -> Result<Option<SessionTransportSnapshot>, RingSessionError> {
        self.ensure_available()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        let Some(cmd_tx) = self.cmd_tx.lock().clone() else {
            return self.worker_failure();
        };
        if cmd_tx.send(RingMsg::Transport { reply_tx }).is_err() {
            return self.worker_failure();
        }
        reply_rx.recv().map_or_else(|_| self.worker_failure(), Ok)
    }

    /// What a deck joins this session with. The ring backend drives the
    /// device callback's processor.
    pub(crate) fn binding(&self) -> SessionOutputView {
        self.view().output.clone()
    }

    fn view(&self) -> &RootView {
        self.view
            .get()
            .expect("invariant: a started ring session holds its view")
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

    pub(crate) fn pre_arm_error(&self) -> Result<Option<RingRenderError>, RingSessionError> {
        self.ensure_available()?;
        Ok(self.probe.pre_arm_error())
    }

    /// `no_block`: explicit shutdown joins the dedicated ring-session worker.
    #[kithara::allow_block]
    pub(crate) fn shutdown(&self) -> Result<(), RingSessionError> {
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

    pub(crate) fn start(config: ManualRingConfig) -> Result<Self, RingSessionError> {
        Self::start_with(config, |_| Ok(()))
    }

    pub(crate) fn start_count(&self) -> Result<usize, RingSessionError> {
        self.ensure_available()?;
        Ok(self.probe.start_count())
    }

    /// `no_block`: startup waits for the dedicated ring-session worker to finish arming.
    #[kithara::allow_block]
    pub(crate) fn start_with<F>(
        config: ManualRingConfig,
        setup: F,
    ) -> Result<Self, RingSessionError>
    where
        F: FnOnce(&mut FirewheelContext) -> Result<(), RingSessionError> + Send + 'static,
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
            probe,
            cmd_tx: Mutex::new(Some(cmd_tx)),
            credit_gate: Mutex::new(()),
            lifecycle_gate: Mutex::new(()),
            reader: Mutex::new(reader),
            snapshot: Mutex::new(RingSnapshot::default()),
            terminal_error: Mutex::new(None),
            view: OnceLock::new(),
            worker: Mutex::new(Some(worker)),
        };
        match ready_rx.recv() {
            Ok(Ok((snapshot, view))) => {
                *session.snapshot.lock() = snapshot;
                let _ = session.view.set(view);
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

impl Drop for ManualRingSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn ring_session_thread(
    cmd_rx: &mpsc::Receiver<RingMsg>,
    ready_tx: &mpsc::Sender<Result<(RingSnapshot, RootView), RingSessionError>>,
    backend_config: RingBackendConfig,
    session_rate: NonZeroU32,
    probe: RingBackendProbe,
    setup: RingSetup,
) {
    let mut backend_config = Some(backend_config);
    let mut state =
        GraphSession::<RingBackend>::with_sample_rate(session_rate, move |ctx, _sample_rate| {
            let config = backend_config
                .take()
                .ok_or_else(|| String::from("ring backend cannot be restarted"))?;
            let mut backend = RingBackend::start(ctx, config).map_err(|error| error.to_string())?;
            match backend.render_block(0) {
                Err(RingRenderError::NotArmed) => {
                    probe.record_pre_arm_error(RingRenderError::NotArmed);
                }
                Err(error) => return Err(format!("unexpected pre-arm render result: {error}")),
                Ok(()) => return Err(String::from("pre-arm ring render was accepted")),
            }
            backend.arm();
            Ok(backend)
        });
    let ready = bootstrap(&mut state, setup)
        .and_then(|()| snapshot(&mut state))
        .map(|snapshot| (snapshot, state.view()));
    let is_ready = ready.is_ok();
    if ready_tx.send(ready).is_err() || !is_ready {
        return;
    }
    for message in cmd_rx.iter() {
        match message {
            RingMsg::Tick { reply_tx } => {
                let _ = reply_tx.send(state.tick());
            }
            RingMsg::Install { id, bus, reply_tx } => {
                let _ = reply_tx.send(state.install(id, bus));
            }
            RingMsg::Remove { id, reply_tx } => {
                let _ = reply_tx.send(state.remove(id));
            }
            RingMsg::Configure {
                change,
                at,
                reply_tx,
            } => {
                let _ = reply_tx.send(state.configure(change, at));
            }
            RingMsg::Credit { blocks, reply_tx } => {
                let _ = reply_tx.send(credit_blocks(&mut state, blocks));
            }
            RingMsg::Transport { reply_tx } => {
                let _ = reply_tx.send(state.transport());
            }
            RingMsg::Shutdown => return,
        }
    }
}

fn bootstrap(
    state: &mut GraphSession<RingBackend>,
    setup: RingSetup,
) -> Result<(), RingSessionError> {
    state
        .install(BeatGridId::allocate()?, EventBus::default())
        .map_err(|error| RingSessionError::Setup(error.to_string()))?;
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
    let raw_clock = {
        let ctx = state.ctx_mut().ok_or(RingSessionError::NotStarted)?;
        ctx.update()
            .map_err(|error| RingSessionError::Update(format!("{error:?}")))?;
        ctx.audio_clock().samples.0
    };
    let clock_samples =
        u64::try_from(raw_clock).map_err(|_| RingSessionError::NegativeClock(raw_clock))?;
    state
        .stream_mut()
        .ok_or(RingSessionError::NotStarted)?
        .render_block(clock_samples)?;
    snapshot(state)
}

fn snapshot(state: &mut GraphSession<RingBackend>) -> Result<RingSnapshot, RingSessionError> {
    let committed_frames = state
        .stream_mut()
        .ok_or(RingSessionError::NotStarted)?
        .committed_frames();
    let ctx = state.ctx_mut().ok_or(RingSessionError::NotStarted)?;
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
