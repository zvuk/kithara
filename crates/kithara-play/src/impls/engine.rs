//! Engine implementation backed by the Firewheel audio graph.
//!
//! Graph topology (per slot):
//! ```text
//! PlayerNode[slotN] -> VolPanNode[slotN] -> MasterVolPanNode -> GraphOut
//! ```
//!
//! `FirewheelCtx` is `!Send`, so it lives on a dedicated rayon pool thread.
//! [`EngineImpl`] communicates with that thread via blocking channels,
//! making the public API fully `Send + Sync`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_platform::{Mutex, ThreadPool};
use portable_atomic::AtomicF32;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::{
    error::PlayError,
    events::EngineEvent,
    traits::{dj::crossfade::CrossfadeConfig, engine::Engine},
    types::SlotId,
};

use super::{player_processor::PlayerCmd, shared_player_state::SharedPlayerState};

/// Configuration for the audio engine.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Number of output channels. Default: 2 (stereo).
    pub channels: u16,
    /// Number of EQ bands per slot (unused until Task 6). Default: 10.
    pub eq_bands: usize,
    /// Maximum number of concurrent player slots. Default: 4.
    pub max_slots: usize,
    /// PCM buffer pool for audio-thread scratch buffers.
    ///
    /// When `None`, the global PCM pool is used.
    pub pcm_pool: Option<PcmPool>,
    /// Sample rate passed to `CpalBackend` as a hint. Default: 44100.
    pub sample_rate: u32,
    /// Thread pool for the engine thread and background work.
    ///
    /// When `None`, the global rayon pool is used.
    pub thread_pool: Option<ThreadPool>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            channels: 2,
            eq_bands: 10,
            max_slots: 4,
            pcm_pool: None,
            sample_rate: 44100,
            thread_pool: None,
        }
    }
}

impl EngineConfig {
    /// Set maximum number of concurrent player slots.
    #[must_use]
    pub fn with_max_slots(mut self, max_slots: usize) -> Self {
        self.max_slots = max_slots;
        self
    }

    /// Set sample rate hint for `CpalBackend`.
    #[must_use]
    pub fn with_sample_rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// Set number of output channels.
    #[must_use]
    pub fn with_channels(mut self, channels: u16) -> Self {
        self.channels = channels;
        self
    }

    /// Set number of EQ bands per slot.
    #[must_use]
    pub fn with_eq_bands(mut self, eq_bands: usize) -> Self {
        self.eq_bands = eq_bands;
        self
    }

    /// Set PCM buffer pool for audio-thread scratch buffers.
    ///
    /// When not set, the global PCM pool is used.
    #[must_use]
    pub fn with_pcm_pool(mut self, pool: PcmPool) -> Self {
        self.pcm_pool = Some(pool);
        self
    }

    /// Set thread pool for the engine thread and background work.
    ///
    /// When not set, the global rayon pool is used.
    #[must_use]
    pub fn with_thread_pool(mut self, pool: ThreadPool) -> Self {
        self.thread_pool = Some(pool);
        self
    }
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

/// Responses sent back from the engine thread.
enum Reply {
    Ok,
    SlotAllocated(SlotId, kanal::Sender<PlayerCmd>, Arc<SharedPlayerState>),
    SampleRate(u32),
    Err(String),
}

/// Handle for a slot, providing command channel and shared state.
pub(crate) struct SlotHandle {
    pub(crate) slot_id: SlotId,
    pub(crate) cmd_tx: kanal::Sender<PlayerCmd>,
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
    cmd_tx: kanal::Sender<Cmd>,
    /// Receiver half of the reply channel from the engine thread.
    reply_rx: kanal::Receiver<Reply>,

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
        let (reply_tx, reply_rx) = kanal::bounded(8);
        let (done_tx, done_rx) = kanal::bounded(1);

        let max_slots = config.max_slots;
        let thread_pool = config.thread_pool.clone().unwrap_or_default();
        let pcm_pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| pcm_pool().clone());

        thread_pool.spawn(move || {
            engine_thread(&cmd_rx, &reply_tx, max_slots, pcm_pool);
            let _ = done_tx.send(());
        });

        Self {
            config,
            cmd_tx,
            reply_rx,
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
        self.cmd_tx
            .send(cmd)
            .map_err(|_| PlayError::Internal("engine thread gone".into()))?;
        self.reply_rx
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
    #[expect(dead_code, reason = "will be used when player polls position/duration")]
    pub(crate) fn slot_shared_state(&self, slot: SlotId) -> Option<Arc<SharedPlayerState>> {
        self.slot_handles
            .lock()
            .iter()
            .find(|h| h.slot_id == slot)
            .map(|h| Arc::clone(&h.shared_state))
    }
}

impl Drop for EngineImpl {
    fn drop(&mut self) {
        // Ask the engine thread to shut down.
        let _ = self.cmd_tx.send(Cmd::Shutdown);
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
        let Reply::SlotAllocated(slot_id, cmd_tx, shared_state) = reply else {
            return Err(PlayError::Internal(
                "unexpected reply from engine thread".into(),
            ));
        };

        self.active_slots.lock().push(slot_id);
        self.slot_handles.lock().push(SlotHandle {
            slot_id,
            cmd_tx,
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
    slot_id: SlotId,
    player_node_id: firewheel::node::NodeID,
    vol_pan_node_id: firewheel::node::NodeID,
}

/// Mutable state owned by the engine thread.
struct EngineThreadState {
    ctx: Option<firewheel::FirewheelCtx<firewheel::cpal::CpalBackend>>,
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
    cmd_rx: &kanal::Receiver<Cmd>,
    reply_tx: &kanal::Sender<Reply>,
    max_slots: usize,
    pcm_pool: PcmPool,
) {
    let mut state = EngineThreadState {
        ctx: None,
        max_slots,
        master_vol_pan_id: None,
        master_vol_pan_memo: None,
        next_slot_id: 1,
        pcm_pool,
        slot_nodes: Vec::new(),
    };

    while let Ok(cmd) = cmd_rx.recv() {
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
                break;
            }
        };
        let _ = reply_tx.send(reply);
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
            let master_memo = Memo::new(master_node.clone());
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

    let player_node =
        PlayerNode::with_channel(cmd_rx, Arc::clone(&shared_state), state.pcm_pool.clone());
    let player_node_id = fw_ctx.add_node(player_node, None);
    let vol_pan_node_id = fw_ctx.add_node(VolumePanNode::from_volume(Volume::Linear(1.0)), None);

    if let Err(e) = fw_ctx.connect(player_node_id, vol_pan_node_id, &[(0, 0), (1, 1)], false) {
        return Reply::Err(format!("connect player->volpan failed: {e}"));
    }

    // Connect per-slot volume node to the master volume node.
    let Some(master_id) = state.master_vol_pan_id else {
        return Reply::Err("master volume node not initialised".into());
    };
    if let Err(e) = fw_ctx.connect(vol_pan_node_id, master_id, &[(0, 0), (1, 1)], false) {
        return Reply::Err(format!("connect volpan->master_vol failed: {e}"));
    }

    if let Err(e) = fw_ctx.update() {
        warn!(?slot_id, "graph update after allocate_slot failed: {e:?}");
    }

    state.slot_nodes.push(SlotNodes {
        slot_id,
        player_node_id,
        vol_pan_node_id,
    });

    Reply::SlotAllocated(slot_id, cmd_tx, shared_state)
}

fn handle_release_slot(state: &mut EngineThreadState, slot: SlotId) -> Reply {
    let Some(ref mut fw_ctx) = state.ctx else {
        return Reply::Err("engine not running".into());
    };

    let Some(idx) = state.slot_nodes.iter().position(|s| s.slot_id == slot) else {
        return Reply::Err(format!("slot not found: {slot:?}"));
    };

    let entry = state.slot_nodes.remove(idx);

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
mod tests {
    use super::*;

    fn make_engine() -> EngineImpl {
        EngineImpl::new(EngineConfig::default())
    }

    #[test]
    fn engine_config_defaults() {
        let config = EngineConfig::default();
        assert_eq!(config.channels, 2);
        assert_eq!(config.eq_bands, 10);
        assert_eq!(config.max_slots, 4);
        assert_eq!(config.sample_rate, 44100);
    }

    #[test]
    fn engine_config_default_thread_pool_is_none() {
        let config = EngineConfig::default();
        assert!(config.thread_pool.is_none());
    }

    #[test]
    fn engine_config_builder() {
        let pool = ThreadPool::with_num_threads(1).unwrap();
        let config = EngineConfig::default()
            .with_max_slots(8)
            .with_sample_rate(48000)
            .with_channels(1)
            .with_eq_bands(5)
            .with_thread_pool(pool);
        assert_eq!(config.max_slots, 8);
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 1);
        assert!(config.thread_pool.is_some());
        assert_eq!(config.eq_bands, 5);
    }

    #[test]
    fn engine_not_running_initially() {
        let engine = make_engine();
        assert!(!engine.is_running());
    }

    #[test]
    fn engine_slot_count_zero_initially() {
        let engine = make_engine();
        assert_eq!(engine.slot_count(), 0);
        assert_eq!(engine.max_slots(), 4);
    }

    #[test]
    fn engine_subscribe_works() {
        let engine = make_engine();
        let _rx = engine.subscribe();
    }

    #[test]
    fn engine_master_volume_default() {
        let engine = make_engine();
        assert!((engine.master_volume() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn engine_set_master_volume() {
        let engine = make_engine();
        engine.set_master_volume(0.5);
        assert!((engine.master_volume() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn engine_set_master_volume_clamps() {
        let engine = make_engine();
        engine.set_master_volume(2.0);
        assert!((engine.master_volume() - 1.0).abs() < f32::EPSILON);
        engine.set_master_volume(-0.5);
        assert!(engine.master_volume().abs() < f32::EPSILON);
    }

    #[test]
    fn engine_set_master_volume_emits_event() {
        let engine = make_engine();
        let mut rx = engine.subscribe();
        engine.set_master_volume(0.75);
        let event = rx.try_recv().unwrap();
        assert!(
            matches!(event, EngineEvent::MasterVolumeChanged { volume } if (volume - 0.75).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn engine_stop_when_not_running_returns_error() {
        let engine = make_engine();
        let err = engine.stop().unwrap_err();
        assert!(matches!(err, PlayError::EngineNotRunning));
    }

    #[test]
    fn engine_allocate_slot_when_not_running_returns_error() {
        let engine = make_engine();
        let err = engine.allocate_slot().unwrap_err();
        assert!(matches!(err, PlayError::EngineNotRunning));
    }

    #[test]
    fn engine_release_slot_when_not_running_returns_error() {
        let engine = make_engine();
        let err = engine.release_slot(SlotId(99)).unwrap_err();
        assert!(matches!(err, PlayError::EngineNotRunning));
    }

    #[test]
    fn engine_active_slots_empty_initially() {
        let engine = make_engine();
        assert!(engine.active_slots().is_empty());
    }

    #[test]
    fn engine_crossfade_stub_returns_not_ready() {
        let engine = make_engine();
        let err = engine
            .crossfade(SlotId(1), SlotId(2), CrossfadeConfig::default())
            .unwrap_err();
        assert!(matches!(err, PlayError::NotReady));
    }

    #[test]
    fn engine_cancel_crossfade_stub_returns_no_crossfade() {
        let engine = make_engine();
        let err = engine.cancel_crossfade().unwrap_err();
        assert!(matches!(err, PlayError::NoCrossfade));
    }

    #[test]
    fn engine_is_crossfading_returns_false() {
        let engine = make_engine();
        assert!(!engine.is_crossfading());
    }

    #[test]
    fn engine_config_thread_pool_used_by_engine() {
        let pool = ThreadPool::with_num_threads(1).unwrap();
        let config = EngineConfig::default().with_thread_pool(pool);
        // Engine should accept a custom thread pool without panicking.
        let engine = EngineImpl::new(config);
        assert!(!engine.is_running());
    }

    #[test]
    fn engine_master_sample_rate_returns_config_when_stopped() {
        let config = EngineConfig {
            sample_rate: 48000,
            ..Default::default()
        };
        let engine = EngineImpl::new(config);
        assert_eq!(engine.master_sample_rate(), 48000);
    }

    #[test]
    fn engine_master_channels_returns_config() {
        let engine = make_engine();
        assert_eq!(engine.master_channels(), 2);
    }

    // Tests that require actual audio hardware should be marked #[ignore].
    // They are run explicitly during local development or on hardware-capable CI.

    #[test]
    #[ignore = "requires audio hardware"]
    fn engine_start_stop_roundtrip() {
        let engine = make_engine();
        engine.start().unwrap();
        assert!(engine.is_running());
        engine.stop().unwrap();
        assert!(!engine.is_running());
    }

    #[test]
    #[ignore = "requires audio hardware"]
    fn engine_allocate_and_release_slot() {
        let engine = make_engine();
        engine.start().unwrap();

        let slot_id = engine.allocate_slot().unwrap();
        assert_eq!(engine.slot_count(), 1);
        assert!(engine.active_slots().contains(&slot_id));

        engine.release_slot(slot_id).unwrap();
        assert_eq!(engine.slot_count(), 0);

        engine.stop().unwrap();
    }

    #[test]
    #[ignore = "requires audio hardware"]
    fn engine_arena_full_error() {
        let config = EngineConfig {
            max_slots: 1,
            ..Default::default()
        };
        let engine = EngineImpl::new(config);
        engine.start().unwrap();

        let _slot1 = engine.allocate_slot().unwrap();
        let result = engine.allocate_slot();
        assert!(matches!(result, Err(PlayError::ArenaFull)));

        engine.stop().unwrap();
    }
}
