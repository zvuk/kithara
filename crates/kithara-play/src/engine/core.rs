use std::sync::atomic::{AtomicBool, Ordering};

use kithara_audio::AudioWorkerHandle;
use kithara_bufpool::PcmPool;
use kithara_events::EventBus;
use kithara_platform::{
    CancelScope, CancelToken,
    sync::{Arc, Mutex},
    tokio::runtime::Handle as RuntimeHandle,
};
use portable_atomic::AtomicF32;
use ringbuf::traits::{Consumer, Observer, Producer};
use tracing::{debug, info, warn};

use super::{config::EngineConfig, session::default_session_handle, slots::SlotTable};
use crate::{
    api::{EngineEvent, SlotId},
    bridge::{
        PlaybackShared, PlayerCmd, PlayerNotification, SharedEq, SlotControl,
        protocol::RejectedPlayerCmd,
    },
    error::PlayError,
    session::{PlayerId, SessionHandle},
};

type SlotHandle = SlotControl;

pub(crate) enum DeferredPlayerCmdError {
    Unsent(PlayError),
    Rejected(RejectedPlayerCmd),
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct EngineImpl {
    session: SessionHandle,
    running: AtomicBool,
    master_volume: AtomicF32,
    #[field(get)]
    worker: AudioWorkerHandle,
    config: EngineConfig,
    bus: EventBus,
    player_id: Mutex<Option<PlayerId>>,
    slots: Mutex<SlotTable>,
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
            .map_or_else(default_session_handle, SessionHandle::new);
        let max_slots = config.max_slots;
        let resolved_pool = config.pcm_pool.clone();
        let worker_cancel = CancelScope::new(config.cancel.clone()).token();

        Self {
            config,
            bus,
            session,
            master_volume: AtomicF32::new(1.0),
            pcm_pool: resolved_pool,
            player_id: Mutex::default(),
            running: AtomicBool::new(false),
            start_lock: Mutex::new(()),
            slots: Mutex::new(SlotTable::with_capacity(max_slots)),
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

    pub(crate) fn try_send_slot_cmd(
        &self,
        slot: SlotId,
        cmd: PlayerCmd,
    ) -> Result<(), RejectedPlayerCmd> {
        let mut slots = self.slots.lock();
        let result = match slots.get_mut(slot) {
            Some(handle) => handle
                .cmd_tx
                .try_push(cmd)
                .map_err(|command| RejectedPlayerCmd {
                    command: Box::new(command),
                    error: PlayError::SlotChannelFull { slot },
                }),
            None => Err(RejectedPlayerCmd {
                command: Box::new(cmd),
                error: PlayError::SlotNotFound(slot),
            }),
        };
        drop(slots);
        result
    }

    pub(crate) fn try_send_slot_cmd_deferred<F>(
        &self,
        slot: SlotId,
        build: F,
    ) -> Result<(), DeferredPlayerCmdError>
    where
        F: FnOnce() -> Result<PlayerCmd, PlayError>,
    {
        let mut slots = self.slots.lock();
        let handle = slots
            .get_mut(slot)
            .ok_or_else(|| DeferredPlayerCmdError::Unsent(PlayError::SlotNotFound(slot)))?;
        if handle.cmd_tx.is_full() {
            return Err(DeferredPlayerCmdError::Unsent(PlayError::SlotChannelFull {
                slot,
            }));
        }
        let command = build().map_err(DeferredPlayerCmdError::Unsent)?;
        let result = handle.cmd_tx.try_push(command).map_err(|command| {
            DeferredPlayerCmdError::Rejected(RejectedPlayerCmd {
                command: Box::new(command),
                error: PlayError::SlotChannelFull { slot },
            })
        });
        drop(slots);
        result
    }

    pub(crate) fn set_master_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session.set_player_eq_gain(player_id, band, gain_db)
    }

    pub(crate) fn set_slot_volume(&self, slot: SlotId, volume: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session
            .set_player_slot_volume(player_id, slot, volume.clamp(0.0, 1.0))
    }

    pub(crate) fn slot_eq(&self, slot: SlotId) -> Option<SharedEq> {
        self.slots.lock().slot_eq(slot)
    }

    pub(crate) fn slot_playback(&self, slot: SlotId) -> Option<Arc<PlaybackShared>> {
        self.slots.lock().playback(slot)
    }

    pub(crate) fn drain_slot_trash(&self, slot: SlotId) -> bool {
        self.slots.lock().get_mut(slot).is_some_and(|handle| {
            Self::drain_slot_trash_handle(handle);
            true
        })
    }

    fn drain_slot_trash_handle(handle: &mut SlotHandle) {
        while handle.trash_rx.try_pop().is_some() {}
    }

    pub(crate) fn pop_slot_notification(&self, slot: SlotId) -> Option<PlayerNotification> {
        self.slots
            .lock()
            .get_mut(slot)
            .and_then(|handle| handle.notif_rx.try_pop())
    }

    pub(crate) fn tick(&self) -> Result<(), PlayError> {
        self.session.tick()
    }

    pub(crate) fn pcm_pool(&self) -> &PcmPool {
        &self.pcm_pool
    }

    pub(super) fn session(&self) -> &SessionHandle {
        &self.session
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
        self.slots.lock().ids()
    }

    pub fn allocate_slot(&self) -> Result<SlotId, PlayError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        {
            let slots = self.slots.lock();
            if slots.len() >= self.config.max_slots {
                return Err(PlayError::ArenaFull);
            }
        }

        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        let allocated = self.session.allocate_slot(player_id)?;
        let slot_id = allocated.slot;

        self.slots.lock().insert(slot_id, allocated.control);

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
            let slots = self.slots.lock();
            if !slots.contains(slot) {
                return Err(PlayError::SlotNotFound(slot));
            }
        }

        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session.release_slot(player_id, slot)?;

        let _ = self.slots.lock().remove(slot);

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

        self.slots.lock().clear();

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
mod tests {
    use std::cell::Cell;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::bridge::{SharedEq, slot_channels};

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

    #[kithara::test]
    fn deferred_command_is_not_built_for_a_missing_slot() {
        let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
        let built = Cell::new(false);

        let result = engine.try_send_slot_cmd_deferred(SlotId::new(0), || {
            built.set(true);
            Ok(PlayerCmd::SetPaused(false))
        });

        assert!(matches!(
            result,
            Err(DeferredPlayerCmdError::Unsent(PlayError::SlotNotFound(_)))
        ));
        assert!(!built.get());
    }

    #[kithara::test]
    fn deferred_command_is_not_built_for_a_full_slot_lane() {
        let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
        let slot = SlotId::new(0);
        let (_inputs, control) = slot_channels(SharedEq::new(0));
        engine.slots.lock().insert(slot, control);
        while engine
            .try_send_slot_cmd(slot, PlayerCmd::SetPaused(false))
            .is_ok()
        {}
        let built = Cell::new(false);

        let result = engine.try_send_slot_cmd_deferred(slot, || {
            built.set(true);
            Ok(PlayerCmd::SetPaused(false))
        });

        assert!(matches!(
            result,
            Err(DeferredPlayerCmdError::Unsent(
                PlayError::SlotChannelFull { .. }
            ))
        ));
        assert!(!built.get());
    }
}
