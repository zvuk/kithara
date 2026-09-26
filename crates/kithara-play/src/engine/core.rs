use std::sync::atomic::{AtomicBool, Ordering};

use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::PoolRegion;
use kithara_effects::eq::EqBandConfig;
use kithara_events::{EventBus, EventReceiver, EventSet, TrackId};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
};
use kithara_signal::FaderValue;
use kithara_sync::{LoadGeneration, SyncExecutionReject};
use kithara_warp::RenderSnapshot;
use portable_atomic::AtomicF32;
use ringbuf::traits::{Consumer, Observer, Producer};
use tracing::{debug, info};

use super::{config::EngineConfig, slots::SlotTable};
use crate::{
    api::{EngineEvent, SessionDuckingMode, SlotId},
    bridge::{
        PlaybackShared, PlayerNotification, SlotControl,
        sync::{SyncReturn, SyncTicket},
    },
    error::PlayError,
    resource::StagingRecipe,
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
    pub(super) eq_layout: Mutex<Vec<EqBandConfig>>,
    pub(super) registration: Mutex<Option<RegisteredPlayer>>,
    pub(super) slots: Arc<Mutex<SlotTable>>,
    #[field(get, vis = "pub(super)")]
    start_lock: Mutex<()>,
    #[field(get, vis = "pub(crate)")]
    pub(super) session: SessionHandle<S>,
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
            registration: Mutex::default(),
            running: AtomicBool::new(false),
            start_lock: Mutex::new(()),
            slots: Arc::new(Mutex::new(SlotTable::with_capacity(max_slots))),
        }
    }

    pub fn active_slots(&self) -> Vec<SlotId> {
        self.slots.lock().ids()
    }

    pub fn allocate_slot(&self) -> Result<SlotId, PlayError> {
        let _start = self.start_lock.lock();
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
            self.slots
                .lock()
                .begin_close_all()
                .map_err(|slot| PlayError::SlotBusy { slot })?;
            if let Err(error) = self.session.stop_player(player_id) {
                self.slots.lock().abort_close_all();
                return Err(error);
            }
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
}

// Slot resource binding, return custody, and release share the slot table.
impl<S> EngineImpl<S> {
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
        while let Some(returned) = handle.sync_return_rx.try_pop() {
            match returned {
                SyncReturn::Ticket(ticket) => {
                    if let Some(seek) = ticket.resource.seek_handle() {
                        handle.unbind_seek(ticket.item_id, &seek);
                    }
                    if let Some(render) = ticket.resource.render_reader() {
                        handle.unbind_render(ticket.item_id, &render);
                    }
                }
                SyncReturn::Track(track) => {
                    if let Some(seek) = track.seek_handle() {
                        handle.unbind_seek(track.item_id(), &seek);
                    }
                    if let Some(render) = track.render_reader() {
                        handle.unbind_render(track.item_id(), &render);
                    }
                }
                SyncReturn::Tail(tail) => {
                    if let Some(seek) = tail.seek_handle() {
                        handle.unbind_seek(tail.item_id, &seek);
                    }
                    if let Some(render) = tail.render_reader() {
                        handle.unbind_render(tail.item_id, &render);
                    }
                }
            }
        }
    }

    /// Bind a staged lane to the same slot and load that received its resident.
    pub(crate) fn bind_staging(
        &self,
        slot: SlotId,
        recipe: StagingRecipe,
    ) -> Option<StagingRecipe> {
        let gate = self.session.sync_gate()?;
        let slots = Arc::clone(&self.slots);
        Some(recipe.with_handoff(gate, move |ticket: SyncTicket| {
            let mut slots = slots.lock();
            let Some(entry) = slots.entry_mut(slot) else {
                return Err(SyncExecutionReject::Cancelled);
            };
            if entry.closing
                || entry
                    .control
                    .render_binding(ticket.item_id)
                    .map(|(load, _)| load)
                    != Some(ticket.load)
            {
                return Err(SyncExecutionReject::Cancelled);
            }
            if entry.control.sync_tx.vacant_len() == 0 {
                return Err(SyncExecutionReject::Capacity);
            }
            let Some(reader) = ticket.resource.render_reader() else {
                return Err(SyncExecutionReject::Geometry);
            };
            let seek = ticket.resource.seek_handle();
            let item_id = ticket.item_id;
            let load = ticket.load;
            let map = ticket.map;
            if entry.control.sync_tx.try_push(ticket).is_err() {
                unreachable!("sole slot sync producer retained its vacancy");
            }
            entry
                .control
                .bind_sync_resource(item_id, load, map, seek, reader);
            drop(slots);
            Ok(())
        }))
    }

    fn emit(&self, event: EngineEvent) {
        self.bus.publish(event);
    }

    pub(crate) fn eq_band_count(&self) -> usize {
        self.eq_layout.lock().len()
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

    delegate::delegate! {
        to self.config {
            #[field]
            pub const fn max_slots(&self) -> usize;
            #[field(&pools)]
            pub(crate) const fn pools(&self) -> &PoolRegion<S>;
        }
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
        let player_id = self.registered_id().ok_or(PlayError::EngineNotRunning)?;
        {
            let mut slots = self.slots.lock();
            let entry = slots.entry_mut(slot).ok_or(PlayError::SlotNotFound(slot))?;
            if entry.reserved_cmds != 0 || entry.closing {
                return Err(PlayError::SlotBusy { slot });
            }
            entry.closing = true;
            drop(slots);
        }
        if let Err(error) = self.session.release_slot(player_id, slot) {
            if let Some(entry) = self.slots.lock().entry_mut(slot) {
                entry.closing = false;
            }
            return Err(error);
        }
        let _ = self.slots.lock().remove(slot);

        debug!(?slot, player_id, "slot released");
        self.emit(EngineEvent::SlotReleased { slot });
        Ok(())
    }
}

// Session-level controls and observable engine state.
impl<S> EngineImpl<S> {
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
        *self.eq_layout.lock() = eq_layout;
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
        let _start = self.start_lock.lock();
        if !self.running.load(Ordering::Acquire) {
            return Err(PlayError::EngineNotRunning);
        }

        let player_id = self.registered_id().ok_or(PlayError::EngineNotRunning)?;
        self.slots
            .lock()
            .begin_close_all()
            .map_err(|slot| PlayError::SlotBusy { slot })?;
        if let Err(error) = self.session.stop_player(player_id) {
            self.slots.lock().abort_close_all();
            return Err(error);
        }

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
            #[call(render_binding)]
            pub(crate) fn slot_render_binding(
                &self,
                slot: SlotId,
                item_id: TrackId,
            ) -> Option<(LoadGeneration, Option<RenderSnapshot>)>;
        }
    }
}
