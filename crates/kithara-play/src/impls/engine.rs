//! Engine implementation backed by the Firewheel audio graph.
//!
//! Graph topology (per slot):
//! ```text
//! PlayerNode[slotN] -> VolPanNode[slotN] -> EqBridgeNode[slotN] -> MasterVolPanNode -> GraphOut
//! ```
//!
//! `FirewheelCtx` is `!Send`, so it lives on a dedicated rayon pool thread.
//! [`EngineImpl`] communicates with that thread via blocking channels,
//! making the public API fully `Send + Sync`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use derivative::Derivative;
use derive_setters::Setters;
use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_platform::{Mutex, ThreadPool};
use portable_atomic::AtomicF32;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::{
    effect_bridge::EffectBridgeNode,
    player_processor::PlayerCmd,
    shared_eq::{SharedEq, SharedEqEffect},
    shared_player_state::SharedPlayerState,
};
use crate::{
    error::PlayError,
    events::EngineEvent,
    traits::{dj::crossfade::CrossfadeConfig, engine::Engine},
    types::SlotId,
};

/// Configuration for the audio engine.
#[derive(Clone, Debug, Derivative, Setters)]
#[derivative(Default)]
#[setters(prefix = "with_", strip_option)]
pub struct EngineConfig {
    /// Number of output channels. Default: 2 (stereo).
    #[derivative(Default(value = "2"))]
    pub channels: u16,
    /// Number of EQ bands per slot (unused until Task 6). Default: 10.
    #[derivative(Default(value = "10"))]
    pub eq_bands: usize,
    /// Maximum number of concurrent player slots. Default: 4.
    #[derivative(Default(value = "4"))]
    pub max_slots: usize,
    /// PCM buffer pool for audio-thread scratch buffers.
    ///
    /// When `None`, the global PCM pool is used.
    pub pcm_pool: Option<PcmPool>,
    /// Sample rate passed to `CpalBackend` as a hint. Default: 44100.
    #[derivative(Default(value = "44100"))]
    pub sample_rate: u32,
    /// Thread pool for the engine thread and background work.
    ///
    /// When `None`, the global rayon pool is used.
    pub thread_pool: Option<ThreadPool>,
}

/// Commands sent from `EngineImpl` to the engine thread.
enum Cmd {
    Start {
        sample_rate: u32,
        master_volume: f32,
    },
    Stop,
    AllocateSlot,
    ReleaseSlot {
        slot: SlotId,
    },
    /// Update the master volume node in the audio graph.
    SetMasterVolume {
        volume: f32,
    },
    /// Query the actual sample rate from `StreamInfo`.
    QuerySampleRate,
    Shutdown,
}

/// Command envelope with a dedicated reply channel.
struct CmdMsg {
    cmd: Cmd,
    reply_tx: kanal::Sender<Reply>,
}

/// Responses sent back from the engine thread.
enum Reply {
    Ok,
    SlotAllocated(
        SlotId,
        kanal::Sender<PlayerCmd>,
        Arc<SharedPlayerState>,
        SharedEq,
    ),
    SampleRate(u32),
    Err(String),
}

/// Handle for a slot, providing command channel and shared state.
pub(crate) struct SlotHandle {
    pub(crate) slot_id: SlotId,
    pub(crate) cmd_tx: kanal::Sender<PlayerCmd>,
    pub(crate) eq: SharedEq,
    pub(crate) shared_state: Arc<SharedPlayerState>,
}

/// Concrete [`Engine`] implementation backed by Firewheel + CPAL.
///
/// The Firewheel context lives on a dedicated rayon pool thread because it is
/// `!Send`. All graph mutations are serialised through blocking channel
/// commands, making `EngineImpl` itself `Send + Sync`.
pub struct EngineImpl {
    config: EngineConfig,

    /// Sender half of the command channel to the engine thread.
    cmd_tx: kanal::Sender<CmdMsg>,

    /// Per-slot tracking (owned by the main side, mirrored).
    active_slots: Mutex<Vec<SlotId>>,

    /// Per-slot command channels and shared state.
    slot_handles: Mutex<Vec<SlotHandle>>,

    /// Master output volume (linear 0.0 ..= 1.0).
    master_volume: AtomicF32,

    /// Event broadcast channel.
    events_tx: broadcast::Sender<EngineEvent>,

    /// Whether the engine is currently running.
    running: AtomicBool,

    /// Signals when the engine thread has finished (used for join on drop).
    done_rx: kanal::Receiver<()>,
}

impl EngineImpl {
    /// Create a new engine with the given configuration.
    ///
    /// The engine is created in the *stopped* state. Call [`Engine::start`]
    /// to begin audio processing.
    #[must_use]
    pub fn new(config: EngineConfig) -> Self {
        let (events_tx, _) = broadcast::channel(64);

        let (cmd_tx, cmd_rx) = kanal::bounded(8);
        let (done_tx, done_rx) = kanal::bounded(1);

        let max_slots = config.max_slots;
        let eq_bands = config.eq_bands;
        let thread_pool = config.thread_pool.clone().unwrap_or_default();
        let pcm_pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| pcm_pool().clone());

        thread_pool.spawn(move || {
            engine_thread(&cmd_rx, max_slots, eq_bands, pcm_pool);
            let _ = done_tx.send(());
        });

        Self {
            config,
            cmd_tx,
            active_slots: Mutex::new(Vec::new()),
            slot_handles: Mutex::new(Vec::new()),
            master_volume: AtomicF32::new(1.0),
            events_tx,
            running: AtomicBool::new(false),
            done_rx,
        }
    }

    /// Emit an event on the broadcast channel. Silently drops if no
    /// subscribers are connected.
    fn emit(&self, event: EngineEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Send a command and wait for its reply.
    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = kanal::bounded(1);
        self.cmd_tx
            .send(CmdMsg { cmd, reply_tx })
            .map_err(|_| PlayError::Internal("engine thread gone".into()))?;
        reply_rx
            .recv()
            .map_err(|_| PlayError::Internal("engine thread gone (reply)".into()))
    }

    /// Send a command, wait for reply, map `Reply::Err` to `PlayError`.
    fn call_ok(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let reply = self.call(cmd)?;
        if let Reply::Err(msg) = &reply {
            return Err(PlayError::Internal(msg.clone()));
        }
        Ok(reply)
    }

    /// Get the command sender for a slot.
    pub(crate) fn slot_cmd_tx(&self, slot: SlotId) -> Option<kanal::Sender<PlayerCmd>> {
        self.slot_handles
            .lock()
            .iter()
            .find(|h| h.slot_id == slot)
            .map(|h| h.cmd_tx.clone())
    }

    /// Get the shared state for a slot.
    pub(crate) fn slot_shared_state(&self, slot: SlotId) -> Option<Arc<SharedPlayerState>> {
        self.slot_handles
            .lock()
            .iter()
            .find(|h| h.slot_id == slot)
            .map(|h| Arc::clone(&h.shared_state))
    }

    pub(crate) fn slot_eq(&self, slot: SlotId) -> Option<SharedEq> {
        self.slot_handles
            .lock()
            .iter()
            .find(|h| h.slot_id == slot)
            .map(|h| h.eq.clone())
    }
}

impl Drop for EngineImpl {
    fn drop(&mut self) {
        // Ask the engine thread to shut down.
        let (reply_tx, _) = kanal::bounded(1);
        let _ = self.cmd_tx.send(CmdMsg {
            cmd: Cmd::Shutdown,
            reply_tx,
        });
        // Wait for it to finish.
        let _ = self.done_rx.recv();
    }
}

impl Engine for EngineImpl {
    // -- lifecycle -----------------------------------------------------------

    fn start(&self) -> Result<(), PlayError> {
        if self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineAlreadyRunning);
        }

        self.call_ok(Cmd::Start {
            sample_rate: self.config.sample_rate,
            master_volume: self.master_volume.load(Ordering::Relaxed),
        })?;

        self.running.store(true, Ordering::Release);

        info!(
            sample_rate = self.config.sample_rate,
            channels = self.config.channels,
            max_slots = self.config.max_slots,
            "engine started"
        );
        self.emit(EngineEvent::Started);
        Ok(())
    }

    fn stop(&self) -> Result<(), PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        self.call_ok(Cmd::Stop)?;

        // Clear local slot tracking.
        self.active_slots.lock().clear();
        self.slot_handles.lock().clear();

        self.running.store(false, Ordering::Release);
        info!("engine stopped");
        self.emit(EngineEvent::Stopped);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    // -- arena slot management -----------------------------------------------

    fn allocate_slot(&self) -> Result<SlotId, PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        {
            let slots = self.active_slots.lock();
            if slots.len() >= self.config.max_slots {
                return Err(PlayError::ArenaFull);
            }
        }

        let reply = self.call_ok(Cmd::AllocateSlot)?;
        let Reply::SlotAllocated(slot_id, cmd_tx, shared_state, eq) = reply else {
            return Err(PlayError::Internal(
                "unexpected reply from engine thread".into(),
            ));
        };

        self.active_slots.lock().push(slot_id);
        self.slot_handles.lock().push(SlotHandle {
            slot_id,
            cmd_tx,
            eq,
            shared_state,
        });

        debug!(?slot_id, "slot allocated");
        self.emit(EngineEvent::SlotAllocated { slot: slot_id });
        Ok(slot_id)
    }

    fn release_slot(&self, slot: SlotId) -> Result<(), PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        {
            let slots = self.active_slots.lock();
            if !slots.contains(&slot) {
                return Err(PlayError::SlotNotFound(slot));
            }
        }

        self.call_ok(Cmd::ReleaseSlot { slot })?;

        self.active_slots.lock().retain(|s| *s != slot);
        self.slot_handles.lock().retain(|h| h.slot_id != slot);

        debug!(?slot, "slot released");
        self.emit(EngineEvent::SlotReleased { slot });
        Ok(())
    }

    fn active_slots(&self) -> Vec<SlotId> {
        self.active_slots.lock().clone()
    }

    fn slot_count(&self) -> usize {
        self.active_slots.lock().len()
    }

    fn max_slots(&self) -> usize {
        self.config.max_slots
    }

    // -- master output -------------------------------------------------------

    fn master_volume(&self) -> f32 {
        self.master_volume.load(Ordering::Relaxed)
    }

    fn set_master_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.master_volume.store(clamped, Ordering::Relaxed);
        if self.running.load(Ordering::Acquire) {
            let _ = self.call(Cmd::SetMasterVolume { volume: clamped });
        }
        self.emit(EngineEvent::MasterVolumeChanged { volume: clamped });
    }

    fn master_sample_rate(&self) -> u32 {
        if self.running.load(Ordering::Acquire)
            && let Ok(Reply::SampleRate(sr)) = self.call(Cmd::QuerySampleRate)
        {
            return sr;
        }
        self.config.sample_rate
    }

    fn master_channels(&self) -> u16 {
        self.config.channels
    }

    // -- crossfade (stubs) ---------------------------------------------------

    fn crossfade(
        &self,
        _from: SlotId,
        _to: SlotId,
        _config: CrossfadeConfig,
    ) -> Result<(), PlayError> {
        Err(PlayError::NotReady)
    }

    fn cancel_crossfade(&self) -> Result<(), PlayError> {
        Err(PlayError::NoCrossfade)
    }

    fn is_crossfading(&self) -> bool {
        false
    }

    // -- events --------------------------------------------------------------

    fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events_tx.subscribe()
    }
}

/// Internal record keeping the Firewheel node IDs for one slot.
#[derive(Debug, Clone, Copy)]
struct SlotNodes {
    eq_node_id: firewheel::node::NodeID,
    slot_id: SlotId,
    player_node_id: firewheel::node::NodeID,
    vol_pan_node_id: firewheel::node::NodeID,
}

/// Mutable state owned by the engine thread.
struct EngineThreadState {
    ctx: Option<firewheel::FirewheelCtx<firewheel::cpal::CpalBackend>>,
    eq_bands: usize,
    max_slots: usize,
    /// Node ID of the master volume/pan node in the audio graph.
    master_vol_pan_id: Option<firewheel::node::NodeID>,
    /// Memo for diffing master volume changes and sending patches.
    master_vol_pan_memo: Option<firewheel::diff::Memo<firewheel::nodes::volume_pan::VolumePanNode>>,
    next_slot_id: u64,
    pcm_pool: PcmPool,
    slot_nodes: Vec<SlotNodes>,
}

/// The dedicated rayon pool thread that owns the `FirewheelCtx`.
fn engine_thread(
    cmd_rx: &kanal::Receiver<CmdMsg>,
    max_slots: usize,
    eq_bands: usize,
    pcm_pool: PcmPool,
) {
    let mut state = EngineThreadState {
        ctx: None,
        eq_bands,
        max_slots,
        master_vol_pan_id: None,
        master_vol_pan_memo: None,
        next_slot_id: 1,
        pcm_pool,
        slot_nodes: Vec::new(),
    };

    while let Ok(msg) = cmd_rx.recv() {
        let CmdMsg { cmd, reply_tx } = msg;
        let should_shutdown = matches!(cmd, Cmd::Shutdown);
        let reply = match cmd {
            Cmd::Start {
                sample_rate,
                master_volume,
            } => handle_start(&mut state, sample_rate, master_volume),
            Cmd::Stop => handle_stop(&mut state),
            Cmd::AllocateSlot => handle_allocate_slot(&mut state),
            Cmd::ReleaseSlot { slot } => handle_release_slot(&mut state, slot),
            Cmd::SetMasterVolume { volume } => handle_set_master_volume(&mut state, volume),
            Cmd::QuerySampleRate => handle_query_sample_rate(&state),
            Cmd::Shutdown => {
                if let Some(ref mut fw_ctx) = state.ctx {
                    fw_ctx.stop_stream();
                }
                Reply::Ok
            }
        };
        let _ = reply_tx.send(reply);
        if should_shutdown {
            break;
        }
    }
}

fn handle_start(state: &mut EngineThreadState, sample_rate: u32, master_volume: f32) -> Reply {
    use firewheel::{
        FirewheelConfig, Volume, cpal::CpalBackend, diff::Memo, nodes::volume_pan::VolumePanNode,
    };

    if state.ctx.is_some() {
        return Reply::Err("already started".into());
    }

    let fw_config = FirewheelConfig {
        num_graph_outputs: firewheel::channel_config::ChannelCount::STEREO,
        ..FirewheelConfig::default()
    };

    let mut fw_ctx = firewheel::FirewheelCtx::<CpalBackend>::new(fw_config);

    let cpal_config = firewheel::cpal::CpalConfig {
        output: firewheel::cpal::CpalOutputConfig {
            desired_sample_rate: Some(sample_rate),
            ..Default::default()
        },
        ..Default::default()
    };

    match fw_ctx.start_stream(cpal_config) {
        Ok(()) => {
            // Create a master volume node and connect it to the graph output.
            let master_node = VolumePanNode::from_volume(Volume::Linear(master_volume));
            let master_memo = Memo::new(master_node);
            let master_id = fw_ctx.add_node(master_node, None);

            let graph_out = fw_ctx.graph_out_node_id();
            if let Err(e) = fw_ctx.connect(master_id, graph_out, &[(0, 0), (1, 1)], false) {
                return Reply::Err(format!("connect master_vol->graph_out failed: {e}"));
            }

            if let Err(e) = fw_ctx.update() {
                warn!("graph update after master vol_pan creation failed: {e:?}");
            }

            state.master_vol_pan_id = Some(master_id);
            state.master_vol_pan_memo = Some(master_memo);
            state.ctx = Some(fw_ctx);
            state.slot_nodes.clear();
            Reply::Ok
        }
        Err(e) => Reply::Err(format!("failed to start audio stream: {e}")),
    }
}

fn handle_stop(state: &mut EngineThreadState) -> Reply {
    if let Some(ref mut fw_ctx) = state.ctx {
        fw_ctx.stop_stream();
    }
    state.ctx = None;
    state.slot_nodes.clear();
    state.master_vol_pan_id = None;
    state.master_vol_pan_memo = None;
    Reply::Ok
}

fn handle_allocate_slot(state: &mut EngineThreadState) -> Reply {
    use firewheel::{Volume, nodes::volume_pan::VolumePanNode};

    use crate::impls::player_node::PlayerNode;

    let Some(ref mut fw_ctx) = state.ctx else {
        return Reply::Err("engine not running".into());
    };

    if state.slot_nodes.len() >= state.max_slots {
        return Reply::Err("arena full".into());
    }

    let slot_id = SlotId(state.next_slot_id);
    state.next_slot_id += 1;

    // Create command channel and shared state for this slot.
    let (cmd_tx, cmd_rx) = kanal::bounded(32);
    let shared_state = Arc::new(SharedPlayerState::new());
    let shared_eq = SharedEq::new(state.eq_bands);

    let player_node =
        PlayerNode::with_channel(cmd_rx, Arc::clone(&shared_state), state.pcm_pool.clone());
    let sample_rate = fw_ctx
        .stream_info()
        .map_or(44100, |info| info.sample_rate.get());
    let eq_effect = SharedEqEffect::new(shared_eq.clone(), sample_rate, 2);
    let eq_node = EffectBridgeNode::new(Box::new(eq_effect), state.pcm_pool.clone());

    let player_node_id = fw_ctx.add_node(player_node, None);
    let vol_pan_node_id = fw_ctx.add_node(VolumePanNode::from_volume(Volume::Linear(1.0)), None);
    let eq_node_id = fw_ctx.add_node(eq_node, None);

    if let Err(e) = fw_ctx.connect(player_node_id, vol_pan_node_id, &[(0, 0), (1, 1)], false) {
        return Reply::Err(format!("connect player->volpan failed: {e}"));
    }
    if let Err(e) = fw_ctx.connect(vol_pan_node_id, eq_node_id, &[(0, 0), (1, 1)], false) {
        return Reply::Err(format!("connect volpan->eq failed: {e}"));
    }

    // Connect per-slot volume node to the master volume node.
    let Some(master_id) = state.master_vol_pan_id else {
        return Reply::Err("master volume node not initialised".into());
    };
    if let Err(e) = fw_ctx.connect(eq_node_id, master_id, &[(0, 0), (1, 1)], false) {
        return Reply::Err(format!("connect eq->master_vol failed: {e}"));
    }

    if let Err(e) = fw_ctx.update() {
        warn!(?slot_id, "graph update after allocate_slot failed: {e:?}");
    }

    state.slot_nodes.push(SlotNodes {
        eq_node_id,
        slot_id,
        player_node_id,
        vol_pan_node_id,
    });

    Reply::SlotAllocated(slot_id, cmd_tx, shared_state, shared_eq)
}

fn handle_release_slot(state: &mut EngineThreadState, slot: SlotId) -> Reply {
    let Some(ref mut fw_ctx) = state.ctx else {
        return Reply::Err("engine not running".into());
    };

    let Some(idx) = state.slot_nodes.iter().position(|s| s.slot_id == slot) else {
        return Reply::Err(format!("slot not found: {slot:?}"));
    };

    let entry = state.slot_nodes.remove(idx);

    if let Err(e) = fw_ctx.remove_node(entry.eq_node_id) {
        warn!(?slot, ?e, "failed to remove eq node");
    }
    if let Err(e) = fw_ctx.remove_node(entry.vol_pan_node_id) {
        warn!(?slot, ?e, "failed to remove vol_pan node");
    }
    if let Err(e) = fw_ctx.remove_node(entry.player_node_id) {
        warn!(?slot, ?e, "failed to remove player node");
    }

    if let Err(e) = fw_ctx.update() {
        warn!(?slot, "graph update after release_slot failed: {e:?}");
    }

    Reply::Ok
}

fn handle_set_master_volume(state: &mut EngineThreadState, volume: f32) -> Reply {
    use firewheel::Volume;

    let Some(ref mut fw_ctx) = state.ctx else {
        return Reply::Err("engine not running".into());
    };

    let Some(master_id) = state.master_vol_pan_id else {
        return Reply::Err("master volume node not initialised".into());
    };

    let Some(ref mut memo) = state.master_vol_pan_memo else {
        return Reply::Err("master volume memo not initialised".into());
    };

    memo.volume = Volume::Linear(volume);
    {
        let mut queue = fw_ctx.event_queue(master_id);
        memo.update_memo(&mut queue);
    }

    if let Err(e) = fw_ctx.update() {
        warn!("graph update after set_master_volume failed: {e:?}");
    }

    Reply::Ok
}

fn handle_query_sample_rate(state: &EngineThreadState) -> Reply {
    let sr = state
        .ctx
        .as_ref()
        .and_then(|c| c.stream_info())
        .map_or(44100, |si| si.sample_rate.get());
    Reply::SampleRate(sr)
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
