#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use kithara_audio::AudioWorkerHandle;
use kithara_bufpool::PcmPool;
use kithara_events::EventBus;
use kithara_platform::{
    CancelScope, CancelToken, sync::Mutex, tokio::runtime::Handle as RuntimeHandle,
};
use portable_atomic::AtomicF32;
use ringbuf::traits::{Consumer, Producer};
use tracing::{debug, info, warn};

use super::config::EngineConfig;
use crate::{
    api::{EngineEvent, SessionDuckingMode, SlotId},
    bridge::{PlaybackShared, PlayerCmd, PlayerNotification, SharedEq, SlotControl},
    error::PlayError,
    rt::ArenaRegistry,
    session::{PlayerId, SessionHandle, session_client},
};

type SlotHandle = SlotControl;

pub struct EngineImpl {
    session: SessionHandle,
    running: AtomicBool,
    master_volume: AtomicF32,
    worker: AudioWorkerHandle,
    config: EngineConfig,
    bus: EventBus,
    active_slots: Mutex<Vec<SlotId>>,
    player_id: Mutex<Option<PlayerId>>,
    slot_registry: Mutex<ArenaRegistry<SlotId, SlotHandle>>,
    start_lock: Mutex<()>,
    runtime: Option<RuntimeHandle>,
    pcm_pool: PcmPool,
}

impl EngineImpl {
    /// Create a new engine with the given configuration.
    #[must_use]
    pub fn new(mut config: EngineConfig, bus: EventBus) -> Self {
        let session = config
            .session
            .take()
            .map_or_else(|| SessionHandle::new(session_client()), SessionHandle::new);
        let max_slots = config.max_slots;
        let resolved_pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| PcmPool::default().clone());
        let worker_cancel = CancelScope::new(config.cancel.clone()).token();

        Self {
            config,
            bus,
            session,
            active_slots: Mutex::default(),
            master_volume: AtomicF32::new(1.0),
            pcm_pool: resolved_pool,
            player_id: Mutex::default(),
            running: AtomicBool::new(false),
            start_lock: Mutex::new(()),
            slot_registry: Mutex::new(ArenaRegistry::with_capacity(max_slots)),
            worker: AudioWorkerHandle::with_cancel(worker_cancel),
            runtime: RuntimeHandle::try_current().ok(),
        }
    }

    pub(crate) fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub(crate) fn cancel(&self) {
        if let Some(cancel) = &self.config.cancel {
            cancel.cancel();
        }
    }

    pub(crate) fn cancel_token(&self) -> Option<CancelToken> {
        self.config.cancel.clone()
    }

    pub(crate) fn configured_sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    pub(crate) fn eq_band_count(&self) -> usize {
        self.config.eq_layout.len()
    }

    fn emit(&self, event: EngineEvent) {
        self.bus.publish(event);
    }

    fn ensure_player_id(&self) -> Result<PlayerId, PlayError> {
        let mut player_id = self.player_id.lock();
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

    /// Runtime handle captured at engine creation.
    ///
    /// Use when building a shared
    /// [`Downloader`](kithara_stream::dl::Downloader) so its async tasks
    /// land on the same runtime the audio engine observes, then pass the
    /// downloader through [`ResourceConfig::with_downloader`](super::config::ResourceConfig::with_downloader).
    #[must_use]
    pub fn runtime(&self) -> Option<&RuntimeHandle> {
        self.runtime.as_ref()
    }

    pub(crate) fn send_slot_cmd(&self, slot: SlotId, cmd: PlayerCmd) -> Result<(), PlayError> {
        let mut slot_registry = self.slot_registry.lock();
        let result = match slot_registry.get_mut(&slot) {
            Some(handle) => handle
                .cmd_tx
                .try_push(cmd)
                .map_err(|_| PlayError::SlotChannelFull { slot }),
            None => Err(PlayError::SlotNotFound(slot)),
        };
        drop(slot_registry);
        result
    }

    /// Process-wide session ducking mode.
    #[must_use]
    pub fn session_ducking() -> SessionDuckingMode {
        match SessionHandle::new(session_client()).ducking() {
            Ok(mode) => mode,
            Err(err) => {
                warn!(?err, "failed to query session ducking");
                SessionDuckingMode::Off
            }
        }
    }

    pub(crate) fn set_master_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session.set_player_eq_gain(player_id, band, gain_db)
    }

    /// Set process-wide session ducking mode.
    pub fn set_session_ducking(mode: SessionDuckingMode) -> Result<(), PlayError> {
        SessionHandle::new(session_client()).set_ducking(mode)
    }

    pub(crate) fn set_slot_volume(&self, slot: SlotId, volume: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session
            .set_player_slot_volume(player_id, slot, volume.clamp(0.0, 1.0))
    }

    pub(crate) fn slot_eq(&self, slot: SlotId) -> Option<SharedEq> {
        self.slot_registry.lock().get(&slot).map(|h| h.eq.clone())
    }

    pub(crate) fn slot_playback(&self, slot: SlotId) -> Option<Arc<PlaybackShared>> {
        self.slot_registry
            .lock()
            .get(&slot)
            .map(|h| Arc::clone(&h.playback))
    }

    pub(crate) fn drain_slot_trash(&self, slot: SlotId) -> bool {
        self.slot_registry
            .lock()
            .get_mut(&slot)
            .is_some_and(|handle| {
                Self::drain_slot_trash_handle(handle);
                true
            })
    }

    fn drain_slot_trash_handle(handle: &mut SlotHandle) {
        while handle.trash_rx.try_pop().is_some() {}
    }

    pub(crate) fn pop_slot_notification(&self, slot: SlotId) -> Option<PlayerNotification> {
        self.slot_registry
            .lock()
            .get_mut(&slot)
            .and_then(|handle| handle.notif_rx.try_pop())
    }

    pub(crate) fn tick(&self) -> Result<(), PlayError> {
        self.session.tick()
    }

    pub(crate) fn pcm_pool(&self) -> &PcmPool {
        &self.pcm_pool
    }

    /// Shared audio worker handle for this engine.
    ///
    /// Clone and pass to [`ResourceConfig::with_worker`] so all tracks
    /// loaded through this engine share a single decode thread.
    #[must_use]
    pub fn worker(&self) -> &AudioWorkerHandle {
        &self.worker
    }
}

impl Drop for EngineImpl {
    fn drop(&mut self) {
        let player_id = *self.player_id.lock();
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

impl EngineImpl {
    pub fn active_slots(&self) -> Vec<SlotId> {
        self.active_slots.lock().clone()
    }

    pub fn allocate_slot(&self) -> Result<SlotId, PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        {
            let slots = self.active_slots.lock();
            if slots.len() >= self.config.max_slots {
                return Err(PlayError::ArenaFull);
            }
        }

        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        let allocated = self.session.allocate_slot(player_id)?;
        let slot_id = allocated.slot;

        self.active_slots.lock().push(slot_id);
        self.slot_registry.lock().insert(slot_id, allocated.control);

        debug!(?slot_id, player_id, "slot allocated");
        self.emit(EngineEvent::SlotAllocated { slot: slot_id });
        Ok(slot_id)
    }

    pub fn invalidate_audio_route(&self, reason: &str) -> Result<(), PlayError> {
        if !self.running.load(Ordering::Acquire) {
            debug!(
                reason,
                "audio route invalidation ignored while engine is stopped"
            );
            return Ok(());
        }
        self.session.invalidate_audio_route(reason)
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn master_volume(&self) -> f32 {
        self.master_volume.load(Ordering::Relaxed)
    }

    pub fn max_slots(&self) -> usize {
        self.config.max_slots
    }

    pub fn release_slot(&self, slot: SlotId) -> Result<(), PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        {
            let slots = self.active_slots.lock();
            if !slots.contains(&slot) {
                return Err(PlayError::SlotNotFound(slot));
            }
        }

        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session.release_slot(player_id, slot)?;

        self.active_slots.lock().retain(|s| *s != slot);
        let _ = self.slot_registry.lock().remove(&slot);

        debug!(?slot, player_id, "slot released");
        self.emit(EngineEvent::SlotReleased { slot });
        Ok(())
    }

    pub fn start(&self) -> Result<(), PlayError> {
        let _start = self.start_lock.lock();
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

    pub fn stop(&self) -> Result<(), PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session.stop_player(player_id)?;

        self.active_slots.lock().clear();
        self.slot_registry.lock().clear();

        self.running.store(false, Ordering::Release);
        info!(player_id, "engine stopped");
        self.emit(EngineEvent::Stopped);
        Ok(())
    }

    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }

    /// Effective sample rate of the audio host (from Firewheel / `CoreAudio`).
    ///
    /// Returns the config default if the engine is not running yet.
    /// Used to pre-initialise the resampler in `ResourceConfig` so that
    /// `make_sincs` runs during `Audio::new()` (off the worker thread)
    /// instead of lazily on the first `step_track()` call.
    pub fn master_sample_rate(&self) -> u32 {
        if !self.running.load(Ordering::Acquire) {
            return self.config.sample_rate;
        }
        self.session.query_sample_rate(self.config.sample_rate)
    }
}

#[cfg(test)]
pub(crate) fn ducking_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn engine_creates_worker() {
        let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
        let _w = engine.worker();
    }

    #[kithara::test]
    fn engine_worker_is_clonable() {
        let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
        let w1 = engine.worker().clone();
        let w2 = engine.worker().clone();
        w1.wake();
        w2.wake();
    }

    #[kithara::test]
    fn engine_drop_shuts_down_worker() {
        let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
        let worker_clone = engine.worker().clone();
        drop(engine);
        worker_clone.wake();
    }
}
