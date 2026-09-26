use std::sync::atomic::{AtomicBool, Ordering};

use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::PoolRegion;
use kithara_effects::eq::EqBandConfig;
use kithara_events::{EventBus, EventReceiver, EventSet};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_signal::FaderValue;
use kithara_warp::RenderSnapshot;
use portable_atomic::AtomicF32;
use ringbuf::traits::{Consumer, Producer};
use tracing::{debug, info};

use super::{config::EngineConfig, slots::SlotTable};
use crate::{
    api::{EngineEvent, SessionDuckingMode, SlotId},
    bridge::{PlaybackShared, PlayerCmd, PlayerNotification, SlotControl},
    error::PlayError,
    rt::StreamShape,
    session::{RegisteredPlayer, SessionBinding, SessionHandle, SessionSampleRate},
};

type SlotHandle = SlotControl;

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct EngineImpl<S> {
    running: AtomicBool,
    master_volume: AtomicF32,
    pub(super) config: EngineConfig<S>,
    #[field(get, vis = "pub(crate)")]
    pub(super) bus: EventBus,
    pub(super) registration: Mutex<Option<RegisteredPlayer>>,
    slots: Mutex<SlotTable>,
    #[field(get, vis = "pub(super)")]
    start_lock: Mutex<()>,
    #[field(get, vis = "pub(crate)")]
    pub(super) session: SessionHandle<S>,
}

#[cfg(test)]
mod config_tests {
    use std::num::{NonZeroU32, NonZeroUsize};

    use kithara_config::Config as _;
    use kithara_test_utils::kithara;
    use kithara_warp::BeatGridId;

    use super::*;
    use crate::test_pools::{TestPools, pools};

    #[kithara::test]
    fn engine_config_remains_the_live_eq_layout_owner() {
        let config: EngineConfig<TestPools> = EngineConfig::builder()
            .grid_id(BeatGridId::allocate().expect("a grid identity"))
            .pools(pools())
            .sample_rate(NonZeroU32::new(48_000).expect("48000 is not zero"))
            .response_budget_frames(NonZeroUsize::new(448).expect("448 is not zero"))
            .max_slots(3)
            .build();
        let engine = EngineImpl::new(config, EventBus::new(32));

        assert_eq!(engine.config.values().sample_rate.get(), 48_000);
        assert_eq!(engine.config.values().max_slots, 3);
        assert_eq!(engine.config.values().eq_layout.len(), 10);
        engine
            .set_master_eq_layout(kithara_effects::eq::generate_log_spaced_bands(4))
            .expect("unregistered engine accepts its next layout");
        assert_eq!(engine.config.values().eq_layout.len(), 4);
    }
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
        Self {
            config,
            bus,
            session,
            master_volume: AtomicF32::new(1.0),
            registration: Mutex::default(),
            running: AtomicBool::new(false),
            start_lock: Mutex::new(()),
            slots: Mutex::new(SlotTable::with_capacity(max_slots)),
        }
    }

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

        let player_id = self.registered_id().ok_or(PlayError::EngineNotRunning)?;
        let allocated = self.session.allocate_slot(player_id)?;
        let slot_id = allocated.slot;

        self.slots.lock().insert(slot_id, allocated.control);

        debug!(?slot_id, player_id, "slot allocated");
        self.emit(EngineEvent::SlotAllocated { slot: slot_id });
        Ok(slot_id)
    }

    pub(crate) fn attach_session(&self, binding: SessionBinding<S>) -> Result<(), PlayError> {
        self.validate_session_sample_rate(binding.requested_sample_rate().get())?;
        self.session.bind(binding)
    }

    pub(crate) fn begin_slot_seek(&self, slot: SlotId, position: Duration) {
        let slots = self.slots.lock();
        if let Some(handle) = slots.get(slot) {
            handle.begin_seek(position);
        }
        drop(slots);
    }

    pub(crate) fn cancel(&self) {
        if let Some(cancel) = &self.config.cancel {
            cancel.cancel();
        }
    }

    pub(crate) fn cancel_token(&self) -> Option<CancelToken> {
        self.config.cancel.clone()
    }

    /// Explicitly detach this player from its session.
    ///
    /// A failed detach retains the registered identity so the owning Host can
    /// retry or report the still-live member instead of losing lifecycle
    /// ownership. Repeated successful calls are no-ops.
    pub fn close(&self) -> Result<(), PlayError> {
        let _start = self.start_lock.lock();
        let Some(player_id) = self.registered_id() else {
            return Ok(());
        };

        if self.running.load(Ordering::Acquire) {
            self.session.stop_player(player_id)?;
            self.slots.lock().clear();
            self.running.store(false, Ordering::Release);
            self.emit(EngineEvent::Stopped);
        }

        self.session.unregister_player(player_id)?;
        *self.registration.lock() = None;
        Ok(())
    }

    /// Store the desired gain without dispatching: the mixer batch already
    /// actuated the graph.
    pub(crate) fn commit_desired_master_volume(&self, level: f32) {
        self.master_volume.store(level, Ordering::Relaxed);
    }

    pub(crate) const fn configured_sample_rate(&self) -> u32 {
        self.config.sample_rate.get()
    }

    pub(crate) fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        self.session.consumer_wake_mode()
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

    fn emit(&self, event: EngineEvent) {
        self.bus.publish(event);
    }

    pub(crate) fn eq_band_count(&self) -> usize {
        self.config.eq_layout.lock().len()
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

    pub(crate) const fn pools(&self) -> &PoolRegion<S> {
        &self.config.pools
    }

    pub(crate) fn pop_slot_notification(&self, slot: SlotId) -> Option<PlayerNotification> {
        self.slots
            .lock()
            .get_mut(slot)
            .and_then(|handle| handle.notif_rx.try_pop())
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

        let player_id = self.registered_id().ok_or(PlayError::EngineNotRunning)?;
        self.session.release_slot(player_id, slot)?;

        let _ = self.slots.lock().remove(slot);

        debug!(?slot, player_id, "slot released");
        self.emit(EngineEvent::SlotReleased { slot });
        Ok(())
    }

    /// A resource crossing to the audio thread leaves its seek handle here, since seeking takes
    /// locks. Bindings apply only once the command is accepted; the resource releases when it
    /// returns as trash.
    pub(crate) fn send_slot_cmd(&self, slot: SlotId, cmd: PlayerCmd) -> Result<(), PlayError> {
        let mut slots = self.slots.lock();
        let result = match slots.get_mut(slot) {
            Some(handle) => {
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
    pub(crate) fn set_master_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let player_id = self.registered_id().ok_or(PlayError::EngineNotRunning)?;
        self.session.set_player_eq_gain(player_id, band, gain_db)
    }

    pub(crate) fn set_master_eq_layout(
        &self,
        eq_layout: Vec<EqBandConfig>,
    ) -> Result<(), PlayError> {
        let player_id = self.registered_id();
        if let Some(player_id) = player_id {
            self.session
                .set_player_eq_layout(player_id, eq_layout.clone())?;
        }
        *self.config.eq_layout.lock() = eq_layout;
        Ok(())
    }

    pub fn set_session_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        self.session.set_session_ducking(mode)
    }

    pub(crate) fn set_slot_volume(&self, slot: SlotId, volume: f32) -> Result<(), PlayError> {
        let player_id = self.registered_id().ok_or(PlayError::EngineNotRunning)?;
        self.session
            .set_player_slot_volume(player_id, slot, FaderValue::from(volume))
    }

    pub fn start(&self) -> Result<(), PlayError> {
        let _start = self.start_lock.lock();
        if self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineAlreadyRunning);
        }

        let player_id = self.ensure_player_id()?;
        let master_volume = self.master_volume.load(Ordering::Relaxed);
        self.session.start_player(
            player_id,
            master_volume,
            self.config.render_quantum_frames,
            self.config.response_budget_frames,
        )?;

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

        let player_id = self.registered_id().ok_or(PlayError::EngineNotRunning)?;
        self.session.stop_player(player_id)?;

        self.slots.lock().clear();

        self.running.store(false, Ordering::Release);
        info!(player_id, "engine stopped");
        self.emit(EngineEvent::Stopped);
        Ok(())
    }

    pub(crate) fn stream_shape(&self) -> Result<Option<StreamShape>, PlayError> {
        self.session.stream_shape()
    }

    pub fn subscribe<E: EventSet>(&self) -> EventReceiver<E> {
        self.bus.subscribe()
    }

    /// The platform suspended this session's audio output at `tick`.
    pub fn suspend_output(&self, tick: u64) {
        self.session.suspend_output(tick);
    }

    /// The audio-thread tick this session's output was suspended at, while the
    /// platform still holds it.
    pub fn suspended_at(&self) -> Option<u64> {
        self.session.suspended_at()
    }

    pub(crate) fn tick(&self) -> Result<(), PlayError> {
        self.session.tick()
    }

    pub(super) fn validate_session_sample_rate(&self, session: u32) -> Result<(), PlayError> {
        let player = self.configured_sample_rate();
        if player == session {
            Ok(())
        } else {
            Err(PlayError::SessionSampleRateMismatch { player, session })
        }
    }

    delegate::delegate! {
        to self.slots.lock() {
            #[call(playback)]
            pub(crate) fn slot_playback(&self, slot: SlotId) -> Option<Arc<PlaybackShared>>;
            #[call(render_snapshot)]
            pub(crate) fn slot_render_snapshot(&self, slot: SlotId) -> Option<RenderSnapshot>;
        }
    }
}
