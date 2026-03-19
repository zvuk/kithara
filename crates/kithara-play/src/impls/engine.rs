//! Engine implementation backed by a process-wide Firewheel session.
//!
//! Graph topology (per player):
//! ```text
//! PlayerNode[slotN] -> VolPanNode[slotN] -> PlayerEqNode -> PlayerVolPanNode -> SessionDucking -> GraphOut
//! ```

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use derivative::Derivative;
use derive_setters::Setters;
use kithara_audio::{AudioWorkerHandle, EqBandConfig, generate_log_spaced_bands};
use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_platform::{Mutex, tokio::sync::broadcast};
use portable_atomic::AtomicF32;
use ringbuf::{HeapProd, traits::Producer};
use tracing::{debug, info, warn};

use super::{
    arena_registry::ArenaRegistry,
    player_processor::PlayerCmd,
    session_engine::{PlayerId, SessionClient, session_client},
    shared_eq::SharedEq,
    shared_player_state::SharedPlayerState,
};
use crate::{
    error::PlayError,
    events::EngineEvent,
    traits::{dj::crossfade::CrossfadeConfig, engine::Engine},
    types::{SessionDuckingMode, SlotId},
};

/// Configuration for the audio engine.
#[derive(Clone, Debug, Derivative, Setters)]
#[derivative(Default)]
#[setters(prefix = "with_", strip_option)]
pub struct EngineConfig {
    /// Number of output channels. Default: 2 (stereo).
    #[derivative(Default(value = "2"))]
    pub channels: u16,
    /// EQ band layout per player. Default: 10-band log-spaced.
    #[derivative(Default(value = "generate_log_spaced_bands(10)"))]
    pub eq_layout: Vec<EqBandConfig>,
    /// Maximum number of concurrent player slots. Default: 4.
    #[derivative(Default(value = "4"))]
    pub max_slots: usize,
    /// PCM buffer pool for audio-thread scratch buffers.
    ///
    /// When `None`, the global PCM pool is used.
    pub pcm_pool: Option<PcmPool>,
    /// Sample rate passed to the runtime backend as a hint. Default: 44100.
    #[derivative(Default(value = "44100"))]
    pub sample_rate: u32,
}

/// Handle for a slot, providing command channel and shared state.
pub(crate) struct SlotHandle {
    pub(crate) cmd_tx: HeapProd<PlayerCmd>,
    pub(crate) eq: SharedEq,
    pub(crate) shared_state: Arc<SharedPlayerState>,
}

/// Concrete [`Engine`] implementation backed by a process-wide session.
///
/// Multiple `EngineImpl` instances share one CPAL/Firewheel stream while
/// retaining independent per-player graph controls.
pub struct EngineImpl {
    config: EngineConfig,

    /// Per-slot tracking (owned by the main side, mirrored).
    active_slots: Mutex<Vec<SlotId>>,

    /// Event broadcast channel.
    events_tx: broadcast::Sender<EngineEvent>,

    /// Master output volume for this player instance (linear 0.0 ..= 1.0).
    master_volume: AtomicF32,

    /// Resolved PCM pool used when registering this player in the session.
    pcm_pool: PcmPool,

    /// Session player ID allocated lazily on first start.
    player_id: Mutex<Option<PlayerId>>,

    /// Whether this engine/player instance is currently running.
    running: AtomicBool,

    /// Process-wide shared session backend.
    session: Arc<SessionClient>,

    /// Per-slot command channels and shared state.
    slot_registry: Mutex<ArenaRegistry<SlotId, SlotHandle>>,

    /// Shared audio worker for cooperative multi-track decoding.
    ///
    /// All tracks loaded by this engine share this single worker thread.
    worker: AudioWorkerHandle,
}

impl EngineImpl {
    /// Create a new engine with the given configuration.
    ///
    /// The engine is created in the *stopped* state. Call [`Engine::start`]
    /// to begin audio processing.
    #[must_use]
    pub fn new(config: EngineConfig) -> Self {
        let (events_tx, _) = broadcast::channel(64);
        let max_slots = config.max_slots;
        let resolved_pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| pcm_pool().clone());
        let session = session_client();

        Self {
            config,
            active_slots: Mutex::new(Vec::new()),
            events_tx,
            master_volume: AtomicF32::new(1.0),
            pcm_pool: resolved_pool,
            player_id: Mutex::new(None),
            running: AtomicBool::new(false),
            session,
            slot_registry: Mutex::new(ArenaRegistry::with_capacity(max_slots)),
            worker: AudioWorkerHandle::new(),
        }
    }

    /// Process-wide session ducking mode.
    #[must_use]
    pub fn session_ducking() -> SessionDuckingMode {
        match session_client().ducking() {
            Ok(mode) => mode,
            Err(err) => {
                warn!(?err, "failed to query session ducking");
                SessionDuckingMode::Off
            }
        }
    }

    /// Set process-wide session ducking mode.
    pub fn set_session_ducking(mode: SessionDuckingMode) -> Result<(), PlayError> {
        session_client().set_ducking(mode)
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events_tx.send(event);
    }

    fn ensure_player_id(&self) -> Result<PlayerId, PlayError> {
        let mut player_id = self.player_id.lock_sync();
        if let Some(id) = *player_id {
            return Ok(id);
        }

        let id = self
            .session
            .register_player(self.config.eq_layout.clone(), self.pcm_pool.clone())?;
        *player_id = Some(id);
        drop(player_id);
        Ok(id)
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "guard must live through find + try_push for atomicity"
    )]
    pub(crate) fn send_slot_cmd(&self, slot: SlotId, cmd: PlayerCmd) -> Result<(), PlayError> {
        let mut slot_registry = self.slot_registry.lock_sync();
        let Some(handle) = slot_registry.get_mut(&slot) else {
            return Err(PlayError::Internal("slot handle not found".into()));
        };
        handle
            .cmd_tx
            .try_push(cmd)
            .map_err(|_| PlayError::Internal("slot channel full".into()))
    }

    pub(crate) fn slot_eq(&self, slot: SlotId) -> Option<SharedEq> {
        self.slot_registry
            .lock_sync()
            .get(&slot)
            .map(|h| h.eq.clone())
    }

    pub(crate) fn slot_shared_state(&self, slot: SlotId) -> Option<Arc<SharedPlayerState>> {
        self.slot_registry
            .lock_sync()
            .get(&slot)
            .map(|h| Arc::clone(&h.shared_state))
    }

    /// Shared audio worker handle for this engine.
    ///
    /// Clone and pass to [`ResourceConfig::with_worker`] so all tracks
    /// loaded through this engine share a single decode thread.
    #[must_use]
    pub fn worker(&self) -> &AudioWorkerHandle {
        &self.worker
    }

    pub(crate) fn tick(&self) -> Result<(), PlayError> {
        self.session.tick()
    }

    pub(crate) fn set_slot_volume(&self, slot: SlotId, volume: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock_sync()).ok_or(PlayError::EngineNotRunning)?;
        self.session
            .set_player_slot_volume(player_id, slot, volume.clamp(0.0, 1.0))
    }

    pub(crate) fn set_master_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock_sync()).ok_or(PlayError::EngineNotRunning)?;
        self.session.set_player_eq_gain(player_id, band, gain_db)
    }
}

impl Drop for EngineImpl {
    fn drop(&mut self) {
        // Unregister player first — detaches the graph so the audio thread
        // stops reading from PlayerResources before the worker is killed.
        let player_id = *self.player_id.lock_sync();
        if let Some(player_id) = player_id
            && let Err(err) = self.session.unregister_player(player_id)
        {
            warn!(
                ?err,
                player_id, "failed to unregister player from shared session"
            );
        }

        self.worker.shutdown();
    }
}

impl Engine for EngineImpl {
    fn start(&self) -> Result<(), PlayError> {
        if self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineAlreadyRunning);
        }

        let player_id = self.ensure_player_id()?;
        let master_volume = self.master_volume.load(Ordering::Relaxed);
        self.session
            .start_player(player_id, self.config.sample_rate, master_volume)?;

        self.running.store(true, Ordering::Release);

        info!(
            sample_rate = self.config.sample_rate,
            channels = self.config.channels,
            max_slots = self.config.max_slots,
            player_id,
            "engine started"
        );
        self.emit(EngineEvent::Started);
        Ok(())
    }

    fn stop(&self) -> Result<(), PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        let player_id = (*self.player_id.lock_sync()).ok_or(PlayError::EngineNotRunning)?;
        self.session.stop_player(player_id)?;

        self.active_slots.lock_sync().clear();
        self.slot_registry.lock_sync().clear();

        self.running.store(false, Ordering::Release);
        info!(player_id, "engine stopped");
        self.emit(EngineEvent::Stopped);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn allocate_slot(&self) -> Result<SlotId, PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        {
            let slots = self.active_slots.lock_sync();
            if slots.len() >= self.config.max_slots {
                return Err(PlayError::ArenaFull);
            }
        }

        let player_id = (*self.player_id.lock_sync()).ok_or(PlayError::EngineNotRunning)?;
        let (slot_id, cmd_tx, shared_state, eq) = self.session.allocate_slot(player_id)?;

        self.active_slots.lock_sync().push(slot_id);
        self.slot_registry.lock_sync().insert(
            slot_id,
            SlotHandle {
                cmd_tx,
                eq,
                shared_state,
            },
        );

        debug!(?slot_id, player_id, "slot allocated");
        self.emit(EngineEvent::SlotAllocated { slot: slot_id });
        Ok(slot_id)
    }

    fn release_slot(&self, slot: SlotId) -> Result<(), PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        {
            let slots = self.active_slots.lock_sync();
            if !slots.contains(&slot) {
                return Err(PlayError::SlotNotFound(slot));
            }
        }

        let player_id = (*self.player_id.lock_sync()).ok_or(PlayError::EngineNotRunning)?;
        self.session.release_slot(player_id, slot)?;

        self.active_slots.lock_sync().retain(|s| *s != slot);
        let _ = self.slot_registry.lock_sync().remove(&slot);

        debug!(?slot, player_id, "slot released");
        self.emit(EngineEvent::SlotReleased { slot });
        Ok(())
    }

    fn active_slots(&self) -> Vec<SlotId> {
        self.active_slots.lock_sync().clone()
    }

    fn slot_count(&self) -> usize {
        self.active_slots.lock_sync().len()
    }

    fn max_slots(&self) -> usize {
        self.config.max_slots
    }

    fn master_volume(&self) -> f32 {
        self.master_volume.load(Ordering::Relaxed)
    }

    fn set_master_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.master_volume.store(clamped, Ordering::Relaxed);

        if self.running.load(Ordering::Acquire)
            && let Some(player_id) = *self.player_id.lock_sync()
            && let Err(err) = self.session.set_player_master_volume(player_id, clamped)
        {
            warn!(
                ?err,
                player_id,
                volume = clamped,
                "failed to apply player master volume"
            );
        }

        self.emit(EngineEvent::MasterVolumeChanged { volume: clamped });
    }

    fn master_sample_rate(&self) -> u32 {
        if !self.running.load(Ordering::Acquire) {
            return self.config.sample_rate;
        }
        self.session.query_sample_rate(self.config.sample_rate)
    }

    fn master_channels(&self) -> u16 {
        self.config.channels
    }

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

    fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events_tx.subscribe()
    }
}

impl EngineImpl {
    /// Inject a test slot handle without starting the audio session.
    #[cfg(test)]
    pub(crate) fn inject_test_slot(&self, slot_id: SlotId, shared_state: Arc<SharedPlayerState>) {
        use ringbuf::{HeapRb, traits::Split};

        let (cmd_tx, _cmd_rx) = HeapRb::<PlayerCmd>::new(32).split();
        self.active_slots.lock_sync().push(slot_id);
        self.slot_registry.lock_sync().insert(
            slot_id,
            SlotHandle {
                cmd_tx,
                eq: SharedEq::new(0),
                shared_state,
            },
        );
    }
}

#[cfg(test)]
pub(crate) fn ducking_test_lock() -> &'static Mutex<()> {
    use std::sync::OnceLock;
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn engine_creates_worker() {
        let engine = EngineImpl::new(EngineConfig::default());
        // Worker accessor should return a valid handle.
        let _w = engine.worker();
    }

    #[kithara::test]
    fn engine_worker_is_clonable() {
        let engine = EngineImpl::new(EngineConfig::default());
        let w1 = engine.worker().clone();
        let w2 = engine.worker().clone();
        // Both clones should be usable (same underlying worker).
        w1.wake();
        w2.wake();
    }

    #[kithara::test]
    fn engine_drop_shuts_down_worker() {
        let engine = EngineImpl::new(EngineConfig::default());
        let worker_clone = engine.worker().clone();
        drop(engine);
        // Worker should be shut down — wake() is harmless on a dead worker.
        worker_clone.wake();
    }
}
