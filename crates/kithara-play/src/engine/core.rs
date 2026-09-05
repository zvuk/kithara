use std::sync::atomic::{AtomicBool, Ordering};

use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::PoolRegion;
use kithara_events::EventBus;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    time::Duration,
    tokio::runtime::Handle as RuntimeHandle,
};
use kithara_warp::RenderSnapshot;
use portable_atomic::AtomicF32;
use ringbuf::traits::{Consumer, Producer};
use tracing::{debug, info};

use super::{config::EngineConfig, slots::SlotTable};
use crate::{
    api::{EngineEvent, SlotId},
    bridge::{PlaybackShared, PlayerCmd, PlayerNotification, SharedEq, SlotControl},
    effects::eq::EqBandConfig,
    error::PlayError,
    rt::StreamShape,
    session::{PlayerId, SessionBinding, SessionHandle, SessionSampleRate},
};

type SlotHandle = SlotControl;

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct EngineImpl<S> {
    running: AtomicBool,
    master_volume: AtomicF32,
    config: EngineConfig<S>,
    eq_layout: Mutex<Vec<EqBandConfig>>,
    #[field(get, vis = "pub(crate)")]
    bus: EventBus,
    player_id: Mutex<Option<PlayerId>>,
    slots: Mutex<SlotTable>,
    start_lock: Mutex<()>,
    runtime: Option<RuntimeHandle>,
    session: SessionHandle<S>,
}

impl<S> EngineImpl<S> {
    /// Create a new engine with the given configuration.
    #[must_use]
    pub fn new(mut config: EngineConfig<S>, bus: EventBus) -> Self {
        let session = config
            .session
            .take()
            .map_or_else(SessionHandle::pending, SessionHandle::new);
        let max_slots = config.max_slots;
        let eq_layout = Mutex::new(std::mem::take(&mut config.eq_layout));
        Self {
            config,
            eq_layout,
            bus,
            session,
            master_volume: AtomicF32::new(1.0),
            player_id: Mutex::default(),
            running: AtomicBool::new(false),
            start_lock: Mutex::new(()),
            slots: Mutex::new(SlotTable::with_capacity(max_slots)),
            runtime: RuntimeHandle::try_current().ok(),
        }
    }

    pub(crate) fn cancel(&self) {
        if let Some(cancel) = &self.config.cancel {
            cancel.cancel();
        }
    }

    pub(crate) fn attach_session(&self, binding: SessionBinding<S>) -> Result<(), PlayError> {
        self.validate_session_sample_rate(binding.requested_sample_rate()?.get())?;
        self.session.bind(binding)
    }

    fn validate_session_sample_rate(&self, session: u32) -> Result<(), PlayError> {
        let player = self.configured_sample_rate();
        if player == session {
            Ok(())
        } else {
            Err(PlayError::SessionSampleRateMismatch { player, session })
        }
    }

    pub(crate) const fn pools(&self) -> &PoolRegion<S> {
        &self.config.pools
    }

    pub(crate) fn cancel_token(&self) -> Option<CancelToken> {
        self.config.cancel.clone()
    }

    pub(crate) const fn configured_sample_rate(&self) -> u32 {
        self.config.sample_rate.get()
    }

    pub(crate) fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        self.session.consumer_wake_mode()
    }

    pub(crate) fn stream_shape(&self) -> Result<Option<StreamShape>, PlayError> {
        self.session.stream_shape()
    }

    pub(crate) fn drain_slot_trash(&self, slot: SlotId) -> bool {
        self.slots.lock().get_mut(slot).is_some_and(|handle| {
            Self::drain_slot_trash_handle(handle);
            true
        })
    }

    fn drain_slot_trash_handle(handle: &mut SlotHandle) {
        while let Some(track) = handle.trash_rx.try_pop() {
            if let Some(seek) = track.seek_handle() {
                handle.unbind_seek(track.item_id(), &seek);
            }
            if let Some(render) = track.render_reader() {
                handle.unbind_render(track.item_id(), &render);
            }
        }
    }

    pub(crate) fn eq_band_count(&self) -> usize {
        self.eq_layout.lock().len()
    }

    fn emit(&self, event: EngineEvent) {
        self.bus.publish(event);
    }

    pub(super) fn ensure_player_id(&self) -> Result<PlayerId, PlayError> {
        let mut player_id = self.player_id.lock();
        if let Some(id) = *player_id {
            return Ok(id);
        }

        self.validate_session_sample_rate(self.session.requested_sample_rate()?.get())?;
        let id = self.session.register_player(
            self.config.grid_id,
            self.bus.clone(),
            self.eq_layout.lock().clone(),
            self.pools().clone(),
        )?;
        *player_id = Some(id);
        drop(player_id);
        Ok(id)
    }

    pub(crate) fn pop_slot_notification(&self, slot: SlotId) -> Option<PlayerNotification> {
        self.slots
            .lock()
            .get_mut(slot)
            .and_then(|handle| handle.notif_rx.try_pop())
    }

    /// Runtime handle captured at engine creation.
    ///
    /// Use when building a shared
    /// [`Downloader`](kithara_stream::dl::Downloader) so its async tasks
    /// land on the same runtime the audio engine observes, then pass the
    /// downloader through [`ResourceConfig::with_downloader`](super::config::ResourceConfig::with_downloader).
    #[must_use]
    pub const fn runtime(&self) -> Option<&RuntimeHandle> {
        self.runtime.as_ref()
    }

    pub(crate) fn send_slot_cmd(&self, slot: SlotId, cmd: PlayerCmd) -> Result<(), PlayError> {
        let mut slots = self.slots.lock();
        let result = match slots.get_mut(slot) {
            Some(handle) => {
                // A resource crossing to the audio thread leaves its seek handle behind: beginning
                // a seek takes locks, so it stays on this side. Bind only after the command is
                // accepted; the exact resource generation is released when it returns as trash.
                let bindings = match &cmd {
                    PlayerCmd::LoadTrack { resource, item_id } => {
                        Some((*item_id, resource.seek_handle(), resource.render_reader()))
                    }
                    _ => None,
                };
                let result = handle
                    .cmd_tx
                    .try_push(cmd)
                    .map_err(|_| PlayError::SlotChannelFull { slot });
                if result.is_ok()
                    && let Some((item_id, seek, render)) = bindings
                {
                    if let Some(seek) = seek {
                        handle.bind_seek(item_id, seek);
                    }
                    if let Some(render) = render {
                        handle.bind_render(item_id, render);
                    }
                }
                result
            }
            None => Err(PlayError::SlotNotFound(slot)),
        };
        drop(slots);
        result
    }

    pub(crate) fn begin_slot_seek(&self, slot: SlotId, position: Duration) {
        let slots = self.slots.lock();
        if let Some(handle) = slots.get(slot) {
            handle.begin_seek(position);
        }
        drop(slots);
    }

    pub(crate) fn set_master_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session.set_player_eq_gain(player_id, band, gain_db)
    }

    pub(crate) fn set_master_eq_layout(
        &self,
        eq_layout: Vec<EqBandConfig>,
    ) -> Result<(), PlayError> {
        let player_id = *self.player_id.lock();
        if let Some(player_id) = player_id {
            self.session
                .set_player_eq_layout(player_id, eq_layout.clone())?;
        }
        *self.eq_layout.lock() = eq_layout;
        Ok(())
    }

    pub(crate) fn set_slot_volume(&self, slot: SlotId, volume: f32) -> Result<(), PlayError> {
        let player_id = (*self.player_id.lock()).ok_or(PlayError::EngineNotRunning)?;
        self.session
            .set_player_slot_volume(player_id, slot, volume.clamp(0.0, 1.0))
    }

    delegate::delegate! {
        to self.slots.lock() {
            pub(crate) fn slot_eq(&self, slot: SlotId) -> Option<SharedEq>;
            #[call(playback)]
            pub(crate) fn slot_playback(&self, slot: SlotId) -> Option<Arc<PlaybackShared>>;
            #[call(render_snapshot)]
            pub(crate) fn slot_render_snapshot(&self, slot: SlotId) -> Option<RenderSnapshot>;
        }
    }

    pub(crate) fn tick(&self) -> Result<(), PlayError> {
        self.session.tick()
    }
    pub fn active_slots(&self) -> Vec<SlotId> {
        self.slots.lock().ids()
    }

    /// Explicitly detach this player from its session.
    ///
    /// A failed detach retains the registered identity so the owning Host can
    /// retry or report the still-live member instead of losing lifecycle
    /// ownership. Repeated successful calls are no-ops.
    pub fn close(&self) -> Result<(), PlayError> {
        let _start = self.start_lock.lock();
        let Some(player_id) = *self.player_id.lock() else {
            return Ok(());
        };

        if self.running.load(Ordering::Acquire) {
            self.session.stop_player(player_id)?;
            self.slots.lock().clear();
            self.running.store(false, Ordering::Release);
            self.emit(EngineEvent::Stopped);
        }

        self.session.unregister_player(player_id)?;
        *self.player_id.lock() = None;
        Ok(())
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

    /// Store the desired gain without dispatching: the mixer batch already
    /// actuated the graph.
    pub(crate) fn commit_desired_master_volume(&self, level: f32) {
        self.master_volume.store(level, Ordering::Relaxed);
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

    /// Effective sample rate of the audio host (from Firewheel / `CoreAudio`).
    ///
    /// Returns the config default if the engine is not running yet.
    /// Used to pre-initialise the resampler in `ResourceConfig` so that
    /// `make_sincs` runs while the resource is prepared (off the worker thread)
    /// instead of lazily on the first `step_track()` call.
    pub fn master_sample_rate(&self) -> u32 {
        if !self.running.load(Ordering::Acquire) {
            return self.config.sample_rate.get();
        }
        self.session
            .sample_rate()
            .map_or_else(|_| self.config.sample_rate.get(), SessionSampleRate::output)
    }

    pub fn master_volume(&self) -> f32 {
        self.master_volume.load(Ordering::Relaxed)
    }

    pub const fn max_slots(&self) -> usize {
        self.config.max_slots
    }

    #[cfg(any(test, feature = "probe"))]
    pub(super) const fn start_lock(&self) -> &Mutex<()> {
        &self.start_lock
    }

    #[cfg(any(test, feature = "probe"))]
    pub(super) const fn session_handle(&self) -> &SessionHandle<S> {
        &self.session
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
        self.session.start_player(player_id, master_volume)?;

        self.running.store(true, Ordering::Release);

        info!(
            sample_rate = self.config.sample_rate.get(),
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
}
