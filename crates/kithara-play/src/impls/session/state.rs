use std::sync::Arc;

#[rustfmt::skip]
use firewheel::nodes::volume_pan::VolumePanNode;
use firewheel::{
    FirewheelConfig, FirewheelCtx, Volume, backend::AudioBackend, channel_config::ChannelCount,
    diff::Memo, node::NodeID,
};
use kithara_audio::EqBandConfig;
use kithara_bufpool::PcmPool;
use kithara_platform::sync::mpsc;
use ringbuf::{HeapProd, HeapRb, traits::Split};
use tracing::warn;

use super::super::{
    master_eq_node::MasterEqNode, player_node::PlayerNode, player_processor::PlayerCmd,
    shared_eq::SharedEq, shared_player_state::SharedPlayerState,
};
use crate::types::{SessionDuckingMode, SlotId};

pub type PlayerId = u64;

/// Capacity of the per-slot player command ring buffer.
pub const PLAYER_CMD_RINGBUF_CAPACITY: usize = 32;

/// Function pointer that starts a firewheel audio stream with the given
/// sample-rate hint on a context parametrised over backend `B`. Each
/// backend (cpal, web-audio, offline) provides its own implementation.
pub type StartStreamFn<B> = fn(&mut FirewheelCtx<B>, u32) -> Result<(), String>;

#[derive(Debug)]
struct SlotNodes {
    vol_pan_memo: Memo<VolumePanNode>,
    player_node_id: NodeID,
    vol_pan_node_id: NodeID,
    slot_id: SlotId,
}

struct PlayerState {
    master_eq_memo: Option<Memo<MasterEqNode>>,
    master_eq_node_id: Option<NodeID>,
    master_vol_pan_memo: Option<Memo<VolumePanNode>>,
    master_vol_pan_node_id: Option<NodeID>,
    pcm_pool: PcmPool,
    player_id: PlayerId,
    shared_eq: SharedEq,
    eq_layout: Vec<EqBandConfig>,
    slots: Vec<SlotNodes>,
    started: bool,
    master_volume: f32,
    next_slot_id: u64,
}

impl PlayerState {
    fn new(player_id: PlayerId, eq_layout: Vec<EqBandConfig>, pcm_pool: PcmPool) -> Self {
        let band_count = eq_layout.len();
        Self {
            eq_layout,
            pcm_pool,
            player_id,
            master_eq_memo: None,
            master_eq_node_id: None,
            master_volume: 1.0,
            master_vol_pan_memo: None,
            master_vol_pan_node_id: None,
            next_slot_id: 1,
            shared_eq: SharedEq::new(band_count),
            slots: Vec::new(),
            started: false,
        }
    }
}

/// Generic audio session state, parametrised over the firewheel backend.
///
/// Held by the engine worker thread for the lifetime of the session.
/// Production paths use `SessionState<CpalBackend>`; integration tests
/// instantiate `SessionState<TestsOfflineBackend>` to drive the same
/// command-dispatch logic without touching real hardware.
pub struct SessionState<B: AudioBackend> {
    ctx: Option<FirewheelCtx<B>>,
    session_output_memo: Option<Memo<VolumePanNode>>,
    session_output_node_id: Option<NodeID>,
    next_player_id: PlayerId,
    session_ducking: SessionDuckingMode,
    /// Backend-specific stream starter baked in at engine-thread spawn
    /// time. Lets [`ensure_ctx`] start the stream without knowing `B`
    /// concretely.
    start_stream_fn: StartStreamFn<B>,
    players: Vec<PlayerState>,
    sample_rate_hint: u32,
}

impl<B: AudioBackend> SessionState<B> {
    /// Default sample rate hint for the audio session.
    pub const DEFAULT_SAMPLE_RATE: u32 = 44_100;

    /// Build a fresh session state with the given backend stream starter.
    ///
    /// `start_stream_fn` is invoked the first time [`Cmd::StartPlayer`]
    /// runs through [`run_cmd`], so the firewheel context is constructed
    /// lazily on the worker thread.
    #[must_use]
    pub fn new(start_stream_fn: StartStreamFn<B>) -> Self {
        Self {
            start_stream_fn,
            ctx: None,
            next_player_id: 1,
            players: Vec::new(),
            sample_rate_hint: Self::DEFAULT_SAMPLE_RATE,
            session_ducking: SessionDuckingMode::Off,
            session_output_memo: None,
            session_output_node_id: None,
        }
    }

    /// Mutable access to the active firewheel context, if a backend has
    /// been installed via [`Cmd::StartPlayer`]. Returns `None` before
    /// any player has started.
    ///
    /// Used by integration-test offline backends to call `ctx.update()`
    /// and `ctx.active_backend_mut()` between command batches.
    pub fn ctx_mut(&mut self) -> Option<&mut FirewheelCtx<B>> {
        self.ctx.as_mut()
    }
}

/// Commands dispatched from `EngineImpl` to the audio session worker.
///
/// Backends consume `Cmd` values from a channel and reply via
/// `mpsc::Sender<Reply>` — see [`CmdMsg`] and [`super::SessionDispatcher`].
/// The canonical dispatch is [`run_cmd`], which works for any backend that
/// implements `firewheel::AudioBackend`.
#[non_exhaustive]
pub enum Cmd {
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

/// Wire envelope used by `EngineImpl` to push a [`Cmd`] onto the worker
/// thread and receive its [`Reply`] back over an `mpsc::Sender<Reply>`.
pub struct CmdMsg {
    /// Command to dispatch on the audio session.
    pub cmd: Cmd,
    /// One-shot reply channel; the worker sends exactly one `Reply`.
    pub reply_tx: mpsc::Sender<Reply>,
}

/// Replies emitted by [`run_cmd`] in response to a [`Cmd`].
///
/// Most callers go through [`super::SessionDispatcher`]'s typed methods,
/// which unpack `Reply` variants into domain values and surface mismatches
/// as `PlayError::Internal`.
#[non_exhaustive]
pub enum Reply {
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

/// Tuple yielded by [`super::SessionDispatcher::allocate_slot`] — the slot
/// id plus the per-slot command channel and shared state. Tests use the
/// shape directly when wiring up an offline session.
pub type AllocatedSlot = (
    SlotId,
    HeapProd<PlayerCmd>,
    Arc<SharedPlayerState>,
    SharedEq,
);

fn ducking_gain(mode: SessionDuckingMode) -> f32 {
    /// Gain applied during soft ducking.
    const DUCKING_GAIN_SOFT: f32 = 0.4;

    /// Gain applied during hard ducking.
    const DUCKING_GAIN_HARD: f32 = 0.2;

    match mode {
        SessionDuckingMode::Off => 1.0,
        SessionDuckingMode::Soft => DUCKING_GAIN_SOFT,
        SessionDuckingMode::Hard => DUCKING_GAIN_HARD,
    }
}

fn player_index<B: AudioBackend>(
    state: &SessionState<B>,
    player_id: PlayerId,
) -> Result<usize, String> {
    state
        .players
        .iter()
        .position(|player| player.player_id == player_id)
        .ok_or_else(|| format!("player not found: {player_id}"))
}

/// Canonical command-dispatch loop body. Generic over the firewheel
/// backend so production cpal paths and integration-test offline paths
/// share the same code.
///
/// Backends call this from their worker thread for every received
/// [`CmdMsg`] and forward the [`Reply`] over the message's
/// `reply_tx`. The function never panics; any internal failure is
/// surfaced as `Reply::Err(...)`.
pub fn run_cmd<B: AudioBackend>(state: &mut SessionState<B>, cmd: Cmd) -> Reply {
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

pub(super) fn ensure_ctx<B: AudioBackend>(
    state: &mut SessionState<B>,
    sample_rate: u32,
) -> Result<(), String> {
    if state.ctx.is_none() {
        let config = FirewheelConfig {
            num_graph_outputs: ChannelCount::STEREO,
            ..FirewheelConfig::default()
        };
        let mut ctx = FirewheelCtx::<B>::new(config);
        (state.start_stream_fn)(&mut ctx, sample_rate)?;
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

fn start_player<B: AudioBackend>(
    state: &mut SessionState<B>,
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

fn stop_player<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    stop_player_idx(state, idx)
}

fn stop_player_idx<B: AudioBackend>(state: &mut SessionState<B>, idx: usize) -> Result<(), String> {
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

fn remove_player_graph<B: AudioBackend>(fw_ctx: &mut FirewheelCtx<B>, player: &mut PlayerState) {
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

fn shutdown_if_idle<B: AudioBackend>(state: &mut SessionState<B>) {
    if state.players.iter().all(|player| !player.started) {
        if let Some(ref mut fw_ctx) = state.ctx {
            fw_ctx.stop_stream();
        }
        state.ctx = None;
        state.session_output_node_id = None;
        state.session_output_memo = None;
    }
}

fn unregister_player<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    if state.players[idx].started {
        stop_player_idx(state, idx)?;
    }
    state.players.remove(idx);
    Ok(())
}

fn allocate_slot<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
) -> Result<Reply, String> {
    let idx = player_index(state, player_id)?;
    if !state.players[idx].started {
        return Err("player not running".into());
    }

    let (fw_ctx, master_eq_id) = match (&mut state.ctx, state.players[idx].master_eq_node_id) {
        (None, _) => return Err("session context is not initialised".into()),
        (Some(_), None) => return Err("player master eq node is not initialised".into()),
        (Some(fw_ctx), Some(master_eq_id)) => (fw_ctx, master_eq_id),
    };

    let slot_id = SlotId::new(state.players[idx].next_slot_id);
    state.players[idx].next_slot_id += 1;

    let (cmd_tx, cmd_rx) = HeapRb::<PlayerCmd>::new(PLAYER_CMD_RINGBUF_CAPACITY).split();
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

fn release_slot<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
    slot: SlotId,
) -> Result<(), String> {
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

fn set_player_master_volume<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
    volume: f32,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    let master_volume = volume.clamp(0.0, 1.0);
    state.players[idx].master_volume = master_volume;
    if !state.players[idx].started {
        return Ok(());
    }

    let player = &mut state.players[idx];
    match (
        &mut state.ctx,
        player.master_vol_pan_node_id,
        &mut player.master_vol_pan_memo,
    ) {
        (None, _, _) => Err("session context is not initialised".into()),
        (Some(_), None, _) => Err("player master vol node is not initialised".into()),
        (Some(_), Some(_), None) => Err("player master vol memo is not initialised".into()),
        (Some(fw_ctx), Some(master_id), Some(memo)) => {
            memo.volume = Volume::Linear(master_volume);
            let mut queue = fw_ctx.event_queue(master_id);
            memo.update_memo(&mut queue);
            Ok(())
        }
    }
}

fn set_player_slot_volume<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
    slot: SlotId,
    volume: f32,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    if !state.players[idx].started {
        return Err("player not running".into());
    }

    let slot_nodes = state.players[idx]
        .slots
        .iter_mut()
        .find(|s| s.slot_id == slot);
    match (&mut state.ctx, slot_nodes) {
        (_, None) => Err(format!("slot not found: {slot:?}")),
        (None, Some(_)) => Err("session context is not initialised".into()),
        (Some(fw_ctx), Some(slot_nodes)) => {
            slot_nodes.vol_pan_memo.volume = Volume::Linear(volume.clamp(0.0, 1.0));
            let mut queue = fw_ctx.event_queue(slot_nodes.vol_pan_node_id);
            slot_nodes.vol_pan_memo.update_memo(&mut queue);
            Ok(())
        }
    }
}

fn set_player_eq_gain<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
    band: usize,
    gain_db: f32,
) -> Result<(), String> {
    let idx = player_index(state, player_id)?;
    if !state.players[idx].started {
        return Err("player not running".into());
    }

    let player = &mut state.players[idx];
    match (
        &mut state.ctx,
        player.master_eq_node_id,
        &mut player.master_eq_memo,
    ) {
        (None, _, _) => Err("session context is not initialised".into()),
        (Some(_), None, _) => Err("player master eq node is not initialised".into()),
        (Some(_), Some(_), None) => Err("player master eq memo is not initialised".into()),
        (Some(fw_ctx), Some(master_eq_id), Some(memo)) => {
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
    }
}

fn set_session_ducking<B: AudioBackend>(state: &mut SessionState<B>, mode: SessionDuckingMode) {
    state.session_ducking = mode;
    if let (Some(fw_ctx), Some(session_id), Some(memo)) = (
        &mut state.ctx,
        state.session_output_node_id,
        &mut state.session_output_memo,
    ) {
        memo.volume = Volume::Linear(ducking_gain(mode));
        let mut queue = fw_ctx.event_queue(session_id);
        memo.update_memo(&mut queue);
    }
}
