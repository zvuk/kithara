#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use std::sync::LazyLock;
#[cfg(target_arch = "wasm32")]
use std::{num::NonZeroU32, sync::atomic::Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::{sync::OnceLock, time::Duration};

use firewheel::{
    FirewheelConfig, Volume, diff::Memo, node::NodeID, nodes::volume_pan::VolumePanNode,
};
use kithara_audio::EqBandConfig;
use kithara_bufpool::PcmPool;
use kithara_platform::{Mutex, sync::mpsc};
#[cfg(not(target_arch = "wasm32"))]
use ringbuf::{
    HeapCons,
    traits::{Consumer, Producer},
};
use ringbuf::{HeapProd, HeapRb, traits::Split};
use tracing::warn;

use super::{
    master_eq_node::MasterEqNode, player_node::PlayerNode, player_processor::PlayerCmd,
    shared_eq::SharedEq, shared_player_state::SharedPlayerState,
};
use crate::{
    error::PlayError,
    types::{SessionDuckingMode, SlotId},
};

pub(crate) type PlayerId = u64;

#[cfg(not(target_arch = "wasm32"))]
type RuntimeBackend = firewheel::cpal::CpalBackend;
#[cfg(target_arch = "wasm32")]
type RuntimeBackend = firewheel_web_audio::WebAudioBackend;

type RuntimeCtx = firewheel::FirewheelCtx<RuntimeBackend>;

#[derive(Debug)]
struct SlotNodes {
    slot_id: SlotId,
    player_node_id: NodeID,
    vol_pan_memo: Memo<VolumePanNode>,
    vol_pan_node_id: NodeID,
}

struct PlayerState {
    eq_layout: Vec<EqBandConfig>,
    master_eq_memo: Option<Memo<MasterEqNode>>,
    master_eq_node_id: Option<NodeID>,
    master_volume: f32,
    master_vol_pan_memo: Option<Memo<VolumePanNode>>,
    master_vol_pan_node_id: Option<NodeID>,
    next_slot_id: u64,
    pcm_pool: PcmPool,
    player_id: PlayerId,
    shared_eq: SharedEq,
    slots: Vec<SlotNodes>,
    started: bool,
}

impl PlayerState {
    fn new(player_id: PlayerId, eq_layout: Vec<EqBandConfig>, pcm_pool: PcmPool) -> Self {
        let band_count = eq_layout.len();
        Self {
            eq_layout,
            master_eq_memo: None,
            master_eq_node_id: None,
            master_volume: 1.0,
            master_vol_pan_memo: None,
            master_vol_pan_node_id: None,
            next_slot_id: 1,
            pcm_pool,
            player_id,
            shared_eq: SharedEq::new(band_count),
            slots: Vec::new(),
            started: false,
        }
    }
}

struct SessionState {
    ctx: Option<RuntimeCtx>,
    next_player_id: PlayerId,
    players: Vec<PlayerState>,
    sample_rate_hint: u32,
    session_ducking: SessionDuckingMode,
    session_output_memo: Option<Memo<VolumePanNode>>,
    session_output_node_id: Option<NodeID>,
}

impl SessionState {
    fn new() -> Self {
        Self {
            ctx: None,
            next_player_id: 1,
            players: Vec::new(),
            sample_rate_hint: 44_100,
            session_ducking: SessionDuckingMode::Off,
            session_output_memo: None,
            session_output_node_id: None,
        }
    }
}

enum Cmd {
    RegisterPlayer {
        eq_layout: Vec<EqBandConfig>,
        pcm_pool: PcmPool,
    },
    UnregisterPlayer {
        player_id: PlayerId,
    },
    StartPlayer {
        master_volume: f32,
        player_id: PlayerId,
        sample_rate: u32,
    },
    StopPlayer {
        player_id: PlayerId,
    },
    AllocateSlot {
        player_id: PlayerId,
    },
    ReleaseSlot {
        player_id: PlayerId,
        slot: SlotId,
    },
    SetPlayerMasterVolume {
        player_id: PlayerId,
        volume: f32,
    },
    SetPlayerSlotVolume {
        player_id: PlayerId,
        slot: SlotId,
        volume: f32,
    },
    SetPlayerEqGain {
        band: usize,
        gain_db: f32,
        player_id: PlayerId,
    },
    SetSessionDucking {
        mode: SessionDuckingMode,
    },
    SessionDucking,
    QuerySampleRate,
    Tick,
}

struct CmdMsg {
    cmd: Cmd,
    reply_tx: mpsc::Sender<Reply>,
}

enum Reply {
    Ok,
    PlayerRegistered(PlayerId),
    SessionDucking(SessionDuckingMode),
    SlotAllocated(
        SlotId,
        HeapProd<PlayerCmd>,
        Arc<SharedPlayerState>,
        SharedEq,
    ),
    SampleRate(u32),
    Err(String),
}

pub(crate) struct SessionClient {
    #[cfg(not(target_arch = "wasm32"))]
    cmd_tx: Mutex<HeapProd<CmdMsg>>,
    /// `true` = main thread (direct state access via thread-local).
    /// `false` = Worker (remote channel proxy).
    #[cfg(target_arch = "wasm32")]
    local: bool,
}

impl SessionClient {
    #[cfg(not(target_arch = "wasm32"))]
    fn push_cmd(&self, msg: CmdMsg) -> Result<(), PlayError> {
        let mut pending = msg;
        loop {
            match self.cmd_tx.lock_sync().try_push(pending) {
                Ok(()) => return Ok(()),
                Err(returned) => {
                    pending = returned;
                    kithara_platform::thread::sleep(Duration::from_micros(100));
                }
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.push_cmd(CmdMsg { cmd, reply_tx })
            .map_err(|_| PlayError::Internal("session thread gone".into()))?;
        reply_rx
            .recv_sync()
            .map_err(|_| PlayError::Internal("session thread gone (reply)".into()))
    }

    #[cfg(target_arch = "wasm32")]
    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        if self.local {
            WASM_SESSION_STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                let state = state
                    .as_mut()
                    .ok_or_else(|| PlayError::Internal("local session state missing".into()))?;
                Ok(run_cmd(state, cmd))
            })
        } else {
            let guard = WORKER_CMD_TX.lock_sync();
            let Some(ref tx) = *guard else {
                return Err(PlayError::Internal("worker channel not initialised".into()));
            };
            let tx = tx.clone();
            drop(guard);
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send_sync(CmdMsg { cmd, reply_tx })
                .map_err(|_| PlayError::Internal("session host gone".into()))?;
            reply_rx
                .recv_sync()
                .map_err(|_| PlayError::Internal("session host gone (reply)".into()))
        }
    }

    fn call_ok(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let reply = self.call(cmd)?;
        if let Reply::Err(msg) = &reply {
            return Err(PlayError::Internal(msg.clone()));
        }
        Ok(reply)
    }

    pub(crate) fn allocate_slot(
        &self,
        player_id: PlayerId,
    ) -> Result<
        (
            SlotId,
            HeapProd<PlayerCmd>,
            Arc<SharedPlayerState>,
            SharedEq,
        ),
        PlayError,
    > {
        match self.call_ok(Cmd::AllocateSlot { player_id })? {
            Reply::SlotAllocated(slot_id, cmd_tx, shared_state, eq) => {
                Ok((slot_id, cmd_tx, shared_state, eq))
            }
            _ => Err(PlayError::Internal(
                "unexpected reply for session allocate slot".into(),
            )),
        }
    }

    pub(crate) fn ducking(&self) -> Result<SessionDuckingMode, PlayError> {
        match self.call_ok(Cmd::SessionDucking)? {
            Reply::SessionDucking(mode) => Ok(mode),
            _ => Err(PlayError::Internal(
                "unexpected reply for session ducking query".into(),
            )),
        }
    }

    pub(crate) fn query_sample_rate(&self, fallback: u32) -> u32 {
        match self.call(Cmd::QuerySampleRate) {
            Ok(Reply::SampleRate(sr)) => sr,
            _ => fallback,
        }
    }

    pub(crate) fn register_player(
        &self,
        eq_layout: Vec<EqBandConfig>,
        pcm_pool: PcmPool,
    ) -> Result<PlayerId, PlayError> {
        match self.call_ok(Cmd::RegisterPlayer {
            eq_layout,
            pcm_pool,
        })? {
            Reply::PlayerRegistered(id) => Ok(id),
            _ => Err(PlayError::Internal(
                "unexpected reply for session player registration".into(),
            )),
        }
    }

    pub(crate) fn release_slot(&self, player_id: PlayerId, slot: SlotId) -> Result<(), PlayError> {
        self.call_ok(Cmd::ReleaseSlot { player_id, slot })
            .map(|_| ())
    }

    pub(crate) fn set_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        self.call_ok(Cmd::SetSessionDucking { mode }).map(|_| ())
    }

    pub(crate) fn set_player_eq_gain(
        &self,
        player_id: PlayerId,
        band: usize,
        gain_db: f32,
    ) -> Result<(), PlayError> {
        self.call_ok(Cmd::SetPlayerEqGain {
            band,
            gain_db,
            player_id,
        })
        .map(|_| ())
    }

    pub(crate) fn set_player_master_volume(
        &self,
        player_id: PlayerId,
        volume: f32,
    ) -> Result<(), PlayError> {
        self.call_ok(Cmd::SetPlayerMasterVolume { player_id, volume })
            .map(|_| ())
    }

    pub(crate) fn set_player_slot_volume(
        &self,
        player_id: PlayerId,
        slot: SlotId,
        volume: f32,
    ) -> Result<(), PlayError> {
        self.call_ok(Cmd::SetPlayerSlotVolume {
            player_id,
            slot,
            volume,
        })
        .map(|_| ())
    }

    pub(crate) fn start_player(
        &self,
        player_id: PlayerId,
        sample_rate: u32,
        master_volume: f32,
    ) -> Result<(), PlayError> {
        self.call_ok(Cmd::StartPlayer {
            master_volume,
            player_id,
            sample_rate,
        })
        .map(|_| ())
    }

    pub(crate) fn stop_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
        self.call_ok(Cmd::StopPlayer { player_id }).map(|_| ())
    }

    pub(crate) fn tick(&self) -> Result<(), PlayError> {
        self.call_ok(Cmd::Tick).map(|_| ())
    }

    pub(crate) fn unregister_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
        self.call_ok(Cmd::UnregisterPlayer { player_id })
            .map(|_| ())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn engine_thread(mut cmd_rx: HeapCons<CmdMsg>) {
    let mut state = SessionState::new();
    loop {
        if let Some(msg) = cmd_rx.try_pop() {
            let CmdMsg { cmd, reply_tx } = msg;
            let reply = run_cmd(&mut state, cmd);
            let _ = reply_tx.send_sync(reply);
            continue;
        }
        kithara_platform::thread::sleep(Duration::from_micros(100));
    }
}

pub(crate) fn session_client() -> Arc<SessionClient> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static SESSION_CLIENT: OnceLock<Arc<SessionClient>> = OnceLock::new();
        SESSION_CLIENT
            .get_or_init(|| {
                let (cmd_tx, cmd_rx) = HeapRb::<CmdMsg>::new(64).split();
                let _ = kithara_platform::spawn(move || engine_thread(cmd_rx));
                Arc::new(SessionClient {
                    cmd_tx: Mutex::new(cmd_tx),
                })
            })
            .clone()
    }

    #[cfg(target_arch = "wasm32")]
    {
        WASM_SESSION_CLIENT.with(|cell| {
            let mut cell = cell.borrow_mut();
            if let Some(client) = cell.as_ref() {
                return client.clone();
            }

            // If the remote channel exists, we are on a Worker thread.
            let is_local = WORKER_CMD_TX.lock_sync().is_none();

            if is_local {
                WASM_SESSION_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    if state.is_none() {
                        *state = Some(SessionState::new());
                    }
                });
                // Pre-warm BRIDGE_PLAYER_STATE: access the thread-local so
                // that TLS destructor registration (which allocates via
                // dlmalloc) happens now — before any Worker is spawned.
                // Without this, the first access during tick_and_poll_remote()
                // can deadlock with a Worker holding the dlmalloc spin lock.
                BRIDGE_PLAYER_STATE.with(|_| {});
            }

            let client = Arc::new(SessionClient { local: is_local });
            *cell = Some(client.clone());
            client
        })
    }
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static WASM_SESSION_CLIENT: RefCell<Option<Arc<SessionClient>>> = const { RefCell::new(None) };
    /// Session state lives in a thread-local so that `SessionClient` itself
    /// is `Send` (needed for the Worker architecture).
    static WASM_SESSION_STATE: RefCell<Option<SessionState>> = const { RefCell::new(None) };
    /// Shared player state captured during slot allocation (for bridge reads).
    static BRIDGE_PLAYER_STATE: RefCell<Option<Arc<SharedPlayerState>>> = const { RefCell::new(None) };
}

/// Sender half of the Worker → main-thread session channel.
///
/// Stored in a global so that Workers can clone it from `session_client()`.
#[cfg(target_arch = "wasm32")]
static WORKER_CMD_TX: LazyLock<Mutex<Option<mpsc::Sender<CmdMsg>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Receiver half (polled on main thread in `tick_and_poll_remote`).
#[cfg(target_arch = "wasm32")]
static WORKER_CMD_RX: LazyLock<Mutex<Option<mpsc::Receiver<CmdMsg>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Initialise the Worker ↔ main-thread session channel.
///
/// Must be called **once** on the main thread **before** any Worker is spawned.
#[cfg(target_arch = "wasm32")]
pub(crate) fn init_worker_channel() {
    let (tx, rx) = mpsc::channel();
    *WORKER_CMD_TX.lock_sync() = Some(tx);
    *WORKER_CMD_RX.lock_sync() = Some(rx);
}

/// Poll pending session commands from Workers and run graph tick.
///
/// Called from the main thread's `requestAnimationFrame` loop.
#[cfg(target_arch = "wasm32")]
pub(crate) fn tick_and_poll_remote() {
    WASM_SESSION_STATE.with(|state_cell| {
        let mut state_opt = state_cell.borrow_mut();
        let Some(ref mut state) = *state_opt else {
            return;
        };

        // 1. Drain remote commands from Workers.
        let rx_guard = WORKER_CMD_RX.lock_sync();
        if let Some(ref rx) = *rx_guard {
            while let Ok(msg) = rx.try_recv() {
                let reply = run_cmd(state, msg.cmd);
                // Capture shared player state for bridge position reads.
                if let Reply::SlotAllocated(_, _, ref shared, _) = reply {
                    BRIDGE_PLAYER_STATE.with(|ps| {
                        *ps.borrow_mut() = Some(Arc::clone(shared));
                    });
                }
                let _ = msg.reply_tx.send_sync(reply);
            }
        }
        drop(rx_guard);

        // 2. Update firewheel graph (tick).
        if let Some(ref mut ctx) = state.ctx {
            if let Err(err) = ctx.update() {
                warn!("session graph update in tick failed: {err:?}");
            }
        }
    });
}

/// Read current playback position from the bridge-captured shared state.
#[cfg(target_arch = "wasm32")]
pub(crate) fn bridge_position_secs() -> f64 {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.position.load(Ordering::Relaxed))
    })
}

/// Read current media duration from the bridge-captured shared state.
#[cfg(target_arch = "wasm32")]
pub(crate) fn bridge_duration_secs() -> f64 {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.duration.load(Ordering::Relaxed))
    })
}

/// Read playing state from the bridge-captured shared state.
#[cfg(target_arch = "wasm32")]
pub(crate) fn bridge_is_playing() -> bool {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|s| s.playing.load(Ordering::Relaxed))
    })
}

/// Read process count from the bridge-captured shared state.
#[cfg(target_arch = "wasm32")]
pub(crate) fn bridge_process_count() -> u64 {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0, |s| s.process_count.load(Ordering::Relaxed))
    })
}

/// Pre-initialise the audio context and AudioWorklet eagerly.
///
/// Must be called on the main thread **after** [`session_client()`] has
/// initialised the session state. Creates the AudioContext (which starts
/// suspended) and kicks off the async AudioWorklet module load. Once the
/// module is ready, `firewheel-web-audio` registers auto-resume listeners
/// for user-gesture events (click, keydown, …). This way the very first
/// user click resumes the context — no second click required.
///
/// Passing `sample_rate = 0` makes `NonZeroU32::new(0)` return `None`,
/// letting the browser pick the default sample rate.
#[cfg(target_arch = "wasm32")]
pub(crate) fn warm_up_audio() {
    WASM_SESSION_STATE.with(|state_cell| {
        let mut state_opt = state_cell.borrow_mut();
        let Some(ref mut state) = *state_opt else {
            return;
        };
        if let Err(err) = ensure_ctx(state, 0) {
            warn!("audio warm-up failed: {err}");
        }
    });
}

fn ducking_gain(mode: SessionDuckingMode) -> f32 {
    match mode {
        SessionDuckingMode::Off => 1.0,
        SessionDuckingMode::Soft => 0.4,
        SessionDuckingMode::Hard => 0.2,
    }
}

fn player_index(state: &SessionState, player_id: PlayerId) -> Result<usize, String> {
    state
        .players
        .iter()
        .position(|player| player.player_id == player_id)
        .ok_or_else(|| format!("player not found: {player_id}"))
}

fn run_cmd(state: &mut SessionState, cmd: Cmd) -> Reply {
    match cmd {
        Cmd::RegisterPlayer {
            eq_layout,
            pcm_pool,
        } => {
            let player_id = state.next_player_id;
            state.next_player_id += 1;
            state
                .players
                .push(PlayerState::new(player_id, eq_layout, pcm_pool));
            Reply::PlayerRegistered(player_id)
        }
        Cmd::UnregisterPlayer { player_id } => match unregister_player(state, player_id) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::StartPlayer {
            master_volume,
            player_id,
            sample_rate,
        } => match start_player(state, player_id, sample_rate, master_volume) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::StopPlayer { player_id } => match stop_player(state, player_id) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::AllocateSlot { player_id } => match allocate_slot(state, player_id) {
            Ok(reply) => reply,
            Err(err) => Reply::Err(err),
        },
        Cmd::ReleaseSlot { player_id, slot } => match release_slot(state, player_id, slot) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetPlayerMasterVolume { player_id, volume } => {
            match set_player_master_volume(state, player_id, volume) {
                Ok(()) => Reply::Ok,
                Err(err) => Reply::Err(err),
            }
        }
        Cmd::SetPlayerSlotVolume {
            player_id,
            slot,
            volume,
        } => match set_player_slot_volume(state, player_id, slot, volume) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetPlayerEqGain {
            band,
            gain_db,
            player_id,
        } => match set_player_eq_gain(state, player_id, band, gain_db) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetSessionDucking { mode } => {
            set_session_ducking(state, mode);
            Reply::Ok
        }
        Cmd::SessionDucking => Reply::SessionDucking(state.session_ducking),
        Cmd::QuerySampleRate => {
            let sample_rate = state
                .ctx
                .as_ref()
                .and_then(|ctx| ctx.stream_info())
                .map_or(state.sample_rate_hint, |si| si.sample_rate.get());
            Reply::SampleRate(sample_rate)
        }
        Cmd::Tick => {
            if let Some(ref mut fw_ctx) = state.ctx
                && let Err(err) = fw_ctx.update()
            {
                return Reply::Err(format!("session graph update failed: {err:?}"));
            }
            Reply::Ok
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_stream(ctx: &mut RuntimeCtx, sample_rate: u32) -> Result<(), String> {
    let config = firewheel::cpal::CpalConfig {
        output: firewheel::cpal::CpalOutputConfig {
            desired_sample_rate: Some(sample_rate),
            ..Default::default()
        },
        ..Default::default()
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}

#[cfg(target_arch = "wasm32")]
fn start_stream(ctx: &mut RuntimeCtx, sample_rate: u32) -> Result<(), String> {
    let config = firewheel_web_audio::WebAudioConfig {
        sample_rate: NonZeroU32::new(sample_rate),
        request_input: false,
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}

fn ensure_ctx(state: &mut SessionState, sample_rate: u32) -> Result<(), String> {
    if state.ctx.is_none() {
        let config = FirewheelConfig {
            num_graph_outputs: firewheel::channel_config::ChannelCount::STEREO,
            ..FirewheelConfig::default()
        };
        let mut ctx = RuntimeCtx::new(config);
        start_stream(&mut ctx, sample_rate)?;
        state.ctx = Some(ctx);
        state.sample_rate_hint = sample_rate;
    }

    if state.session_output_node_id.is_none() {
        let Some(ref mut fw_ctx) = state.ctx else {
            return Err("session context is not initialised".into());
        };
        let session_node =
            VolumePanNode::from_volume(Volume::Linear(ducking_gain(state.session_ducking)));
        let session_memo = Memo::new(session_node);
        let session_id = fw_ctx.add_node(session_node, None);
        let graph_out = fw_ctx.graph_out_node_id();
        fw_ctx
            .connect(session_id, graph_out, &[(0, 0), (1, 1)], false)
            .map_err(|err| format!("connect session output to graph_out failed: {err}"))?;
        if let Err(err) = fw_ctx.update() {
            warn!("session graph update after output init failed: {err:?}");
        }
        state.session_output_node_id = Some(session_id);
        state.session_output_memo = Some(session_memo);
    }

    Ok(())
}

fn start_player(
    state: &mut SessionState,
    player_id: PlayerId,
    sample_rate: u32,
    master_volume: f32,
) -> Result<(), String> {
    ensure_ctx(state, sample_rate)?;

    let idx = player_index(state, player_id)?;
    let Some(ref mut fw_ctx) = state.ctx else {
        return Err("session context is not initialised".into());
    };
    let Some(session_output_id) = state.session_output_node_id else {
        return Err("session output node is not initialised".into());
    };

    let player = &mut state.players[idx];
    if player.started {
        return Err("player already started".into());
    }

    let mut master_eq = MasterEqNode::new(&player.eq_layout);
    for (band, gain) in player.shared_eq.snapshot().into_iter().enumerate() {
        master_eq.set_gain(band, gain);
    }
    let master_eq_memo = Memo::new(master_eq.clone());
    let master_eq_id = fw_ctx.add_node(master_eq, None);

    player.master_volume = master_volume.clamp(0.0, 1.0);
    let master_vol = VolumePanNode::from_volume(Volume::Linear(player.master_volume));
    let master_vol_memo = Memo::new(master_vol);
    let master_vol_id = fw_ctx.add_node(master_vol, None);

    fw_ctx
        .connect(master_eq_id, master_vol_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| format!("connect player master_eq->master_vol failed: {err}"))?;
    fw_ctx
        .connect(master_vol_id, session_output_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| format!("connect player master_vol->session_output failed: {err}"))?;
    if let Err(err) = fw_ctx.update() {
        warn!(player_id, "graph update after player start failed: {err:?}");
    }

    player.master_eq_node_id = Some(master_eq_id);
    player.master_eq_memo = Some(master_eq_memo);
    player.master_vol_pan_node_id = Some(master_vol_id);
    player.master_vol_pan_memo = Some(master_vol_memo);
    player.started = true;

    Ok(())
}

fn stop_player(state: &mut SessionState, player_id: PlayerId) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    stop_player_idx(state, idx)
}

fn stop_player_idx(state: &mut SessionState, idx: usize) -> Result<(), String> {
    if idx >= state.players.len() {
        return Err("player index out of range".into());
    }

    {
        let player = &mut state.players[idx];
        if !player.started {
            return Err("player not running".into());
        }

        if let Some(ref mut fw_ctx) = state.ctx {
            remove_player_graph(fw_ctx, player);
            if let Err(err) = fw_ctx.update() {
                warn!(
                    player_id = player.player_id,
                    "graph update after player stop failed: {err:?}"
                );
            }
        } else {
            clear_player_graph_state(player);
        }
        player.started = false;
    }

    shutdown_if_idle(state);
    Ok(())
}

fn remove_player_graph(fw_ctx: &mut RuntimeCtx, player: &mut PlayerState) {
    let player_id = player.player_id;
    let slots = player.slots.drain(..).collect::<Vec<_>>();
    for slot in slots {
        if let Err(err) = fw_ctx.remove_node(slot.vol_pan_node_id) {
            warn!(player_id, ?err, "failed to remove slot vol_pan node");
        }
        if let Err(err) = fw_ctx.remove_node(slot.player_node_id) {
            warn!(player_id, ?err, "failed to remove slot player node");
        }
    }

    if let Some(master_id) = player.master_vol_pan_node_id.take()
        && let Err(err) = fw_ctx.remove_node(master_id)
    {
        warn!(player_id, ?err, "failed to remove player master vol node");
    }
    if let Some(master_eq_id) = player.master_eq_node_id.take()
        && let Err(err) = fw_ctx.remove_node(master_eq_id)
    {
        warn!(player_id, ?err, "failed to remove player master eq node");
    }
    clear_player_graph_state(player);
}

fn clear_player_graph_state(player: &mut PlayerState) {
    player.master_eq_memo = None;
    player.master_vol_pan_memo = None;
}

fn shutdown_if_idle(state: &mut SessionState) {
    if state.players.iter().all(|player| !player.started) {
        if let Some(ref mut fw_ctx) = state.ctx {
            fw_ctx.stop_stream();
        }
        state.ctx = None;
        state.session_output_node_id = None;
        state.session_output_memo = None;
    }
}

fn unregister_player(state: &mut SessionState, player_id: PlayerId) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    if state.players[idx].started {
        stop_player_idx(state, idx)?;
    }
    state.players.remove(idx);
    Ok(())
}

fn allocate_slot(state: &mut SessionState, player_id: PlayerId) -> Result<Reply, String> {
    let idx = player_index(state, player_id)?;
    if !state.players[idx].started {
        return Err("player not running".into());
    }

    let Some(ref mut fw_ctx) = state.ctx else {
        return Err("session context is not initialised".into());
    };
    let Some(master_eq_id) = state.players[idx].master_eq_node_id else {
        return Err("player master eq node is not initialised".into());
    };

    let slot_id = SlotId(state.players[idx].next_slot_id);
    state.players[idx].next_slot_id += 1;

    let (cmd_tx, cmd_rx) = HeapRb::<PlayerCmd>::new(32).split();
    let shared_state = Arc::new(SharedPlayerState::new());
    let shared_eq = state.players[idx].shared_eq.clone();

    let player_node = PlayerNode::with_channel(
        cmd_rx,
        Arc::clone(&shared_state),
        state.players[idx].pcm_pool.clone(),
    );
    let player_node_id = fw_ctx.add_node(player_node, None);

    let slot_vol_pan = VolumePanNode::from_volume(Volume::Linear(1.0));
    let slot_vol_pan_memo = Memo::new(slot_vol_pan);
    let slot_vol_pan_id = fw_ctx.add_node(slot_vol_pan, None);

    fw_ctx
        .connect(player_node_id, slot_vol_pan_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| format!("connect player->slot_vol_pan failed: {err}"))?;
    fw_ctx
        .connect(slot_vol_pan_id, master_eq_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| format!("connect slot_vol_pan->player_master_eq failed: {err}"))?;
    if let Err(err) = fw_ctx.update() {
        warn!(
            player_id,
            ?slot_id,
            "graph update after slot allocate failed: {err:?}"
        );
    }

    state.players[idx].slots.push(SlotNodes {
        slot_id,
        player_node_id,
        vol_pan_memo: slot_vol_pan_memo,
        vol_pan_node_id: slot_vol_pan_id,
    });

    Ok(Reply::SlotAllocated(
        slot_id,
        cmd_tx,
        shared_state,
        shared_eq,
    ))
}

fn release_slot(state: &mut SessionState, player_id: PlayerId, slot: SlotId) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    if !state.players[idx].started {
        return Err("player not running".into());
    }

    let Some(slot_idx) = state.players[idx]
        .slots
        .iter()
        .position(|s| s.slot_id == slot)
    else {
        return Err(format!("slot not found: {slot:?}"));
    };
    let slot = state.players[idx].slots.remove(slot_idx);

    let Some(ref mut fw_ctx) = state.ctx else {
        return Err("session context is not initialised".into());
    };

    if let Err(err) = fw_ctx.remove_node(slot.vol_pan_node_id) {
        warn!(player_id, ?err, "failed to remove slot vol_pan node");
    }
    if let Err(err) = fw_ctx.remove_node(slot.player_node_id) {
        warn!(player_id, ?err, "failed to remove slot player node");
    }
    if let Err(err) = fw_ctx.update() {
        warn!(player_id, "graph update after slot release failed: {err:?}");
    }

    Ok(())
}

fn set_player_master_volume(
    state: &mut SessionState,
    player_id: PlayerId,
    volume: f32,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    state.players[idx].master_volume = volume.clamp(0.0, 1.0);
    if !state.players[idx].started {
        return Ok(());
    }

    let Some(ref mut fw_ctx) = state.ctx else {
        return Err("session context is not initialised".into());
    };
    let Some(master_id) = state.players[idx].master_vol_pan_node_id else {
        return Err("player master vol node is not initialised".into());
    };
    let master_volume = state.players[idx].master_volume;
    let Some(ref mut memo) = state.players[idx].master_vol_pan_memo else {
        return Err("player master vol memo is not initialised".into());
    };

    memo.volume = Volume::Linear(master_volume);
    let mut queue = fw_ctx.event_queue(master_id);
    memo.update_memo(&mut queue);
    Ok(())
}

fn set_player_slot_volume(
    state: &mut SessionState,
    player_id: PlayerId,
    slot: SlotId,
    volume: f32,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    if !state.players[idx].started {
        return Err("player not running".into());
    }
    let Some(slot_nodes) = state.players[idx]
        .slots
        .iter_mut()
        .find(|s| s.slot_id == slot)
    else {
        return Err(format!("slot not found: {slot:?}"));
    };

    let Some(ref mut fw_ctx) = state.ctx else {
        return Err("session context is not initialised".into());
    };

    slot_nodes.vol_pan_memo.volume = Volume::Linear(volume.clamp(0.0, 1.0));
    let mut queue = fw_ctx.event_queue(slot_nodes.vol_pan_node_id);
    slot_nodes.vol_pan_memo.update_memo(&mut queue);

    Ok(())
}

fn set_player_eq_gain(
    state: &mut SessionState,
    player_id: PlayerId,
    band: usize,
    gain_db: f32,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    if !state.players[idx].started {
        return Err("player not running".into());
    }

    let Some(ref mut fw_ctx) = state.ctx else {
        return Err("session context is not initialised".into());
    };
    let Some(master_eq_id) = state.players[idx].master_eq_node_id else {
        return Err("player master eq node is not initialised".into());
    };
    let Some(ref mut memo) = state.players[idx].master_eq_memo else {
        return Err("player master eq memo is not initialised".into());
    };

    if band >= memo.bands.len() {
        return Err(format!(
            "eq band out of range: {band} (bands: {})",
            memo.bands.len()
        ));
    }

    memo.set_gain(band, gain_db);
    let mut queue = fw_ctx.event_queue(master_eq_id);
    memo.update_memo(&mut queue);
    Ok(())
}

fn set_session_ducking(state: &mut SessionState, mode: SessionDuckingMode) {
    state.session_ducking = mode;
    let Some(ref mut fw_ctx) = state.ctx else {
        return;
    };
    let Some(session_id) = state.session_output_node_id else {
        return;
    };
    let Some(ref mut memo) = state.session_output_memo else {
        return;
    };

    memo.volume = Volume::Linear(ducking_gain(mode));
    let mut queue = fw_ctx.event_queue(session_id);
    memo.update_memo(&mut queue);
}
