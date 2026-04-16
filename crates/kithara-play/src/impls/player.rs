//! Concrete Player implementation managing an items queue.
//!
//! `PlayerImpl` tracks a list of [`Resource`] items and exposes play/pause/seek.
//! Owns an [`EngineImpl`] and sends commands to the active slot's processor
//! via the slot's command channel.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use derivative::Derivative;
use derive_setters::Setters;
use kithara_abr::{AbrController, AbrMode, AbrOptions, ThroughputEstimator};
use kithara_audio::{AudioWorkerHandle, EqBandConfig, generate_log_spaced_bands};
use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_events::EventBus;
use kithara_platform::{Mutex, tokio::runtime::Handle as RuntimeHandle};
use portable_atomic::AtomicF32;
use ringbuf::traits::Consumer;
use tracing::{debug, warn};

use super::{
    engine::{EngineConfig, EngineImpl},
    player_processor::PlayerCmd,
    player_resource::PlayerResource,
    player_track::TrackTransition,
};
#[rustfmt::skip]
use crate::traits::engine::Engine;
use crate::{
    error::PlayError,
    events::PlayerEvent,
    impls::{player_notification::PlayerNotification, resource::Resource},
    types::{ActionAtItemEnd, PlayerStatus, SessionDuckingMode, SlotId},
};

/// Minimum playback rate to prevent stalling.
const MIN_PLAYBACK_RATE: f32 = 0.01;

/// Configuration for the player.
#[derive(Clone, Derivative, Setters)]
#[derivative(Default, Debug)]
#[setters(prefix = "with_", strip_option)]
pub struct PlayerConfig {
    /// Shared ABR controller. When `None`, a default one is created.
    #[derivative(Debug = "ignore")]
    pub abr: Option<AbrController<ThroughputEstimator>>,
    /// Root event bus for this player.
    ///
    /// When set, all player/engine/resource events are published here.
    /// Resources receive a `bus.scoped()` child via `prepare_config()`.
    /// When `None`, a default root bus is created.
    #[setters(skip)]
    pub bus: Option<EventBus>,
    /// Crossfade duration in seconds. Default: 1.0.
    #[derivative(Default(value = "1.0"))]
    pub crossfade_duration: f32,
    /// Default playback rate (1.0 = normal). Default: 1.0.
    #[derivative(Default(value = "1.0"))]
    pub default_rate: f32,
    /// EQ band layout. Default: 10-band log-spaced.
    #[derivative(Default(value = "generate_log_spaced_bands(10)"))]
    pub eq_layout: Vec<EqBandConfig>,
    /// Maximum concurrent slots in the engine. Default: 4.
    #[derivative(Default(value = "4"))]
    pub max_slots: usize,
    /// PCM buffer pool for audio-thread scratch buffers.
    ///
    /// Propagated to the underlying [`EngineImpl`]. When `None`, the global
    /// PCM pool is used.
    pub pcm_pool: Option<PcmPool>,
}

/// Concrete Player implementation managing items queue.
///
/// Owns an [`EngineImpl`] and sends commands to the active slot's processor.
/// When `play()` is called, the engine is lazily started and a slot is
/// allocated. The current queue item is taken out of the queue, wrapped in
/// [`PlayerResource`], and sent to the processor via `PlayerCmd::LoadTrack`.
pub struct PlayerImpl {
    config: PlayerConfig,

    action_at_item_end: Mutex<ActionAtItemEnd>,
    bus: EventBus,
    crossfade_duration: AtomicF32,
    current_index: AtomicUsize,
    current_slot: Mutex<Option<SlotId>>,
    default_rate: AtomicF32,
    /// Items drop before engine — Audio tracks unregister from worker
    /// while it is still alive.
    items: Mutex<Vec<Option<Resource>>>,
    muted: AtomicBool,
    pcm_pool: PcmPool,
    /// Shared playback rate propagated to the audio pipeline resampler.
    playback_rate_shared: Arc<AtomicF32>,
    rate: AtomicF32,
    status: Mutex<PlayerStatus>,
    volume: AtomicF32,

    /// Engine drops last — worker shutdown happens after all tracks unregister.
    engine: EngineImpl,
}

impl PlayerImpl {
    /// Create a new player with the given configuration.
    #[must_use]
    pub fn new(mut config: PlayerConfig) -> Self {
        let resolved_pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| pcm_pool().clone());

        let bus = config.bus.clone().unwrap_or_default();

        let engine_config = EngineConfig {
            eq_layout: config.eq_layout.clone(),
            max_slots: config.max_slots,
            pcm_pool: Some(resolved_pool.clone()),
            ..EngineConfig::default()
        };
        let engine = EngineImpl::new(engine_config, bus.clone());
        if config.abr.is_none() {
            config.abr = Some(AbrController::new(AbrOptions::default()));
        }

        Self {
            action_at_item_end: Mutex::new(ActionAtItemEnd::default()),
            crossfade_duration: AtomicF32::new(config.crossfade_duration),
            current_index: AtomicUsize::new(0),
            current_slot: Mutex::new(None),
            default_rate: AtomicF32::new(config.default_rate),
            bus,
            items: Mutex::new(Vec::new()),
            muted: AtomicBool::new(false),
            pcm_pool: resolved_pool,
            playback_rate_shared: Arc::new(AtomicF32::new(config.default_rate)),
            rate: AtomicF32::new(0.0), // starts paused
            status: Mutex::new(PlayerStatus::Unknown),
            volume: AtomicF32::new(1.0),
            engine,
            config,
        }
    }

    /// Shared audio worker handle for this player's engine.
    ///
    /// Clone and pass to [`ResourceConfig::with_worker`] so resources
    /// loaded into this player share a single decode thread.
    #[must_use]
    pub fn worker(&self) -> &AudioWorkerHandle {
        self.engine.worker()
    }

    /// Runtime handle captured by this player's engine.
    ///
    /// Use when building a shared
    /// [`Downloader`](kithara_stream::dl::Downloader) so its async tasks
    /// land on the same runtime the audio engine observes, then pass the
    /// downloader through [`ResourceConfig::with_downloader`](super::config::ResourceConfig::with_downloader).
    #[must_use]
    pub fn runtime(&self) -> Option<&RuntimeHandle> {
        self.engine.runtime()
    }

    /// Apply shared worker, host sample rate, ABR, and bus to a resource
    /// config so the resource integrates with this player's engine.
    ///
    /// Call this before [`Resource::new`] to ensure the resource shares
    /// the player's decode thread and resampler is pre-initialised with
    /// the correct ratio. Callers that want a shared HTTP pool /
    /// tokio runtime must build their own [`Downloader`] (with an
    /// explicit runtime handle if needed) and pass it via
    /// [`ResourceConfig::with_downloader`].
    pub fn prepare_config(&self, config: &mut super::config::ResourceConfig) {
        config.worker = Some(self.engine.worker().clone());
        config.host_sample_rate = std::num::NonZeroU32::new(self.engine.master_sample_rate());
        #[cfg(feature = "hls")]
        if config.abr.is_none() {
            config.abr.clone_from(&self.config.abr);
        }
        if config.bus.is_none() {
            config.bus = Some(self.bus.scoped());
        }
    }

    /// Get the number of items in the queue (including consumed items).
    pub fn item_count(&self) -> usize {
        self.items.lock_sync().len()
    }

    /// Replace a consumed (or existing) resource at the given index.
    ///
    /// Use this to re-load a track that was previously played and consumed
    /// by [`load_current_item`]. Does nothing if `index` is out of bounds.
    pub fn replace_item(&self, index: usize, resource: Resource) {
        let mut items = self.items.lock_sync();
        if index < items.len() {
            items[index] = Some(resource);
            drop(items);
            debug!(index, "item replaced");
        }
    }

    /// Pre-allocate empty slots so `replace_item` can fill them by index.
    pub fn reserve_slots(&self, count: usize) {
        self.items.lock_sync().resize_with(count, || None);
        debug!(count, "slots reserved");
    }

    /// Insert a resource at a specific position, or append to the end.
    pub fn insert(&self, resource: Resource, at_position: Option<usize>) {
        let mut items = self.items.lock_sync();
        let pos = at_position.map_or(items.len(), |i| i.min(items.len()));
        items.insert(pos, Some(resource));
        debug!(count = items.len(), pos, "item inserted");
    }

    /// Remove item at index. Returns the removed resource, or `None` if out of bounds
    /// or already consumed.
    pub fn remove_at(&self, index: usize) -> Option<Resource> {
        let mut items = self.items.lock_sync();
        if index >= items.len() {
            return None;
        }
        let removed = items.remove(index);
        // Adjust current_index if needed
        let current = self.current_index.load(Ordering::Relaxed);
        if index < current {
            self.current_index
                .store(current.saturating_sub(1), Ordering::Relaxed);
        } else if index == current && current >= items.len() && !items.is_empty() {
            self.current_index.store(items.len() - 1, Ordering::Relaxed);
        }
        debug!(index, remaining = items.len(), "item removed");
        removed
    }

    /// Remove all items from the queue.
    pub fn remove_all_items(&self) {
        self.items.lock_sync().clear();
        self.current_index.store(0, Ordering::Relaxed);
        self.set_status(PlayerStatus::Unknown);
        debug!("all items removed");
    }

    /// Start playback at the configured default rate.
    pub fn play(&self) {
        let rate = self.default_rate().max(MIN_PLAYBACK_RATE);
        self.rate.store(rate, Ordering::Relaxed);
        self.playback_rate_shared.store(rate, Ordering::Relaxed);

        // Start engine and allocate slot if needed.
        if let Err(e) = self.ensure_engine_started() {
            warn!(?e, "failed to start engine");
            return;
        }
        if let Err(e) = self.ensure_slot() {
            warn!(?e, "failed to allocate slot");
            return;
        }

        // Load current resource into slot and start playback.
        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(self.crossfade_duration()));
        self.load_current_item();
        let _ = self.send_to_slot(PlayerCmd::SetPlaybackRate(rate));
        let _ = self.send_to_slot(PlayerCmd::SetPaused(false));

        self.set_status(PlayerStatus::ReadyToPlay);
        self.bus.publish(PlayerEvent::CurrentItemChanged);
        self.bus.publish(PlayerEvent::RateChanged { rate });
        debug!(rate, "play");
    }

    /// Pause playback (sets rate to 0.0).
    pub fn pause(&self) {
        self.rate.store(0.0, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
        self.bus.publish(PlayerEvent::RateChanged { rate: 0.0 });
        debug!("pause");
    }

    /// Current playback rate (0.0 = paused).
    pub fn rate(&self) -> f32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Set playback rate.
    ///
    /// Updates the local rate and propagates to the audio pipeline resampler
    /// via `PlayerCmd::SetPlaybackRate`. Values below 0.01 are clamped to 0.01.
    pub fn set_rate(&self, rate: f32) {
        let clamped = rate.max(MIN_PLAYBACK_RATE);
        self.rate.store(clamped, Ordering::Relaxed);
        self.playback_rate_shared.store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPlaybackRate(clamped));
        self.bus.publish(PlayerEvent::RateChanged { rate: clamped });
    }

    /// Get shared playback rate atomic for the audio pipeline.
    pub fn playback_rate_shared(&self) -> &Arc<AtomicF32> {
        &self.playback_rate_shared
    }

    /// Default playback rate used by `play()` and `select_item()`.
    pub fn default_rate(&self) -> f32 {
        self.default_rate.load(Ordering::Relaxed)
    }

    /// Set the default playback rate used by `play()` and `select_item()`.
    pub fn set_default_rate(&self, rate: f32) {
        self.default_rate.store(rate, Ordering::Relaxed);
    }

    /// Get current volume (0.0..=1.0).
    pub fn volume(&self) -> f32 {
        self.volume.load(Ordering::Relaxed)
    }

    /// Set volume, clamped to `0.0..=1.0`.
    ///
    /// Applies to this player's slot only (per-instance volume).
    pub fn set_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume.store(clamped, Ordering::Relaxed);
        if !self.is_muted() {
            self.apply_effective_volume(clamped);
        }
        self.bus
            .publish(PlayerEvent::VolumeChanged { volume: clamped });
    }

    /// Get process-wide session ducking mode.
    pub fn session_ducking(&self) -> SessionDuckingMode {
        EngineImpl::session_ducking()
    }

    /// Set process-wide session ducking mode.
    pub fn set_session_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        EngineImpl::set_session_ducking(mode)
    }

    /// Returns `true` if the player is muted.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Set muted state.
    ///
    /// Applies to this player's slot only (per-instance mute).
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
        let effective = if muted { 0.0 } else { self.volume() };
        self.apply_effective_volume(effective);
        self.bus.publish(PlayerEvent::MuteChanged { muted });
    }

    /// Current item index in the queue.
    pub fn current_index(&self) -> usize {
        self.current_index.load(Ordering::Relaxed)
    }

    /// Advance to the next item in the queue.
    ///
    /// Does nothing if the current item is already the last one.
    pub fn advance_to_next_item(&self) {
        let items = self.items.lock_sync();
        let current = self.current_index.load(Ordering::Relaxed);
        if current + 1 < items.len() {
            self.current_index.store(current + 1, Ordering::Relaxed);
            drop(items);
            self.bus.publish(PlayerEvent::CurrentItemChanged);
            debug!(new_index = current + 1, "advanced to next item");
        }
    }

    /// Get current player status.
    pub fn status(&self) -> PlayerStatus {
        *self.status.lock_sync()
    }

    /// Set action to perform when the current item ends.
    pub fn set_action_at_item_end(&self, action: ActionAtItemEnd) {
        *self.action_at_item_end.lock_sync() = action;
    }

    /// Get action to perform when the current item ends.
    pub fn action_at_item_end(&self) -> ActionAtItemEnd {
        *self.action_at_item_end.lock_sync()
    }

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        let clamped = seconds.max(0.0);
        self.crossfade_duration.store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(clamped));
    }

    /// Get crossfade duration in seconds.
    pub fn crossfade_duration(&self) -> f32 {
        self.crossfade_duration.load(Ordering::Relaxed)
    }

    /// Subscribe to player events.
    ///
    /// Returns an [`EventReceiver`](kithara_events::EventReceiver) that delivers
    /// all events published to this player's root bus (player events, engine
    /// events, and resource events from all items).
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }

    /// Root event bus for this player.
    ///
    /// Use `bus().scoped()` to create a child scope for a resource.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Get player configuration.
    pub fn config(&self) -> &PlayerConfig {
        &self.config
    }

    /// Get a reference to the underlying engine.
    pub fn engine(&self) -> &EngineImpl {
        &self.engine
    }

    /// Seek active tracks to position in seconds.
    pub fn seek_seconds(&self, seconds: f64) -> Result<(), PlayError> {
        let slot_id = *self.current_slot.lock_sync();
        let Some(slot_id) = slot_id else {
            return Err(PlayError::NotReady);
        };

        let Some(shared_state) = self.engine.slot_shared_state(slot_id) else {
            return Err(PlayError::SlotNotFound(slot_id));
        };

        let seek_epoch = shared_state.next_seek_epoch();
        shared_state.seek_epoch.store(seek_epoch, Ordering::SeqCst);

        self.send_to_slot(PlayerCmd::Seek {
            seconds: seconds.max(0.0),
            seek_epoch,
        })
    }

    /// Current playback position in seconds.
    pub fn position_seconds(&self) -> Option<f64> {
        let slot_id = (*self.current_slot.lock_sync())?;
        let state = self.engine.slot_shared_state(slot_id)?;
        Some(state.position.load(Ordering::Relaxed))
    }

    /// Current media duration in seconds.
    pub fn duration_seconds(&self) -> Option<f64> {
        let slot_id = (*self.current_slot.lock_sync())?;
        let state = self.engine.slot_shared_state(slot_id)?;
        Some(state.duration.load(Ordering::Relaxed))
    }

    /// Diagnostic: number of times the audio processor's `process()` has been called.
    pub fn process_count(&self) -> u64 {
        let Some(slot_id) = *self.current_slot.lock_sync() else {
            return 0;
        };
        let Some(state) = self.engine.slot_shared_state(slot_id) else {
            return 0;
        };
        state.process_count.load(Ordering::Relaxed)
    }

    /// Returns `true` if the player is in playing state.
    pub fn is_playing(&self) -> bool {
        let Some(slot_id) = *self.current_slot.lock_sync() else {
            return false;
        };
        let Some(state) = self.engine.slot_shared_state(slot_id) else {
            return false;
        };
        state.playing.load(Ordering::Relaxed)
    }

    /// Pump audio backend/runtime state.
    pub fn tick(&self) -> Result<(), PlayError> {
        self.engine.tick()
    }

    /// Drain audio-thread notifications for the active slot.
    pub fn drain_notifications(&self) -> Vec<String> {
        let Some(slot_id) = *self.current_slot.lock_sync() else {
            return Vec::new();
        };
        let Some(state) = self.engine.slot_shared_state(slot_id) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        while let Some(notification) = state.notification_rx.lock_sync().try_pop() {
            out.push(format!("{notification:?}"));
        }
        out
    }

    /// Process audio-thread notifications, emitting `ItemDidPlayToEnd`
    /// when a track finishes via EOF.
    pub fn process_notifications(&self) {
        let Some(slot_id) = *self.current_slot.lock_sync() else {
            return;
        };
        let Some(state) = self.engine.slot_shared_state(slot_id) else {
            return;
        };

        while let Some(notification) = state.notification_rx.lock_sync().try_pop() {
            match notification {
                PlayerNotification::TrackPlaybackStopped(_) => {
                    self.bus.publish(PlayerEvent::ItemDidPlayToEnd);
                }
                other => {
                    tracing::trace!(?other, "unhandled player notification");
                }
            }
        }
    }

    /// Number of EQ bands available for this player.
    pub fn eq_band_count(&self) -> usize {
        self.config.eq_layout.len()
    }

    /// Get EQ gain for a band in dB.
    pub fn eq_gain(&self, band: usize) -> Option<f32> {
        let slot_id = (*self.current_slot.lock_sync())?;
        self.engine.slot_eq(slot_id).and_then(|eq| eq.gain(band))
    }

    /// Set EQ gain for a band in dB.
    pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        let slot_id = (*self.current_slot.lock_sync())
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        let eq = self
            .engine
            .slot_eq(slot_id)
            .ok_or_else(|| PlayError::Internal("eq state not found".into()))?;
        let clamped = eq.set_gain(band, gain_db)?;
        self.engine.set_master_eq_gain(band, clamped)
    }

    /// Reset EQ gains to 0 dB for all bands.
    pub fn reset_eq(&self) -> Result<(), PlayError> {
        let slot_id = (*self.current_slot.lock_sync())
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        let eq = self
            .engine
            .slot_eq(slot_id)
            .ok_or_else(|| PlayError::Internal("eq state not found".into()))?;
        eq.reset();
        for band in 0..eq.len() {
            self.engine.set_master_eq_gain(band, 0.0)?;
        }
        Ok(())
    }

    /// Get the shared ABR controller (if configured).
    #[must_use]
    pub fn abr(&self) -> Option<&AbrController<ThroughputEstimator>> {
        self.config.abr.as_ref()
    }

    /// Change ABR mode at runtime. Takes effect on next `decide()`.
    pub fn set_abr_mode(&self, mode: AbrMode) {
        if let Some(ref ctrl) = self.config.abr {
            ctrl.set_mode(mode);
        }
    }

    /// Select and load a queue item by index, using the configured
    /// crossfade duration for the transition.
    pub fn select_item(&self, index: usize, autoplay: bool) -> Result<(), PlayError> {
        self.select_item_with_crossfade(index, autoplay, self.crossfade_duration())
    }

    /// Select and load a queue item by index, applying an explicit
    /// crossfade duration for this one transition only.
    ///
    /// Does not mutate the player-configured crossfade — subsequent
    /// calls to [`select_item`] fall back to [`crossfade_duration`].
    /// Pass `0.0` for an immediate cut (no fade); matches
    /// `AVQueuePlayer`'s manual-selection idiom.
    pub fn select_item_with_crossfade(
        &self,
        index: usize,
        autoplay: bool,
        crossfade_seconds: f32,
    ) -> Result<(), PlayError> {
        let items_len = self.item_count();
        if index >= items_len {
            return Err(PlayError::Internal(format!(
                "item index out of range: {index} (len: {items_len})"
            )));
        }

        self.ensure_engine_started()?;
        self.ensure_slot()?;
        self.current_index.store(index, Ordering::Relaxed);
        self.bus.publish(PlayerEvent::CurrentItemChanged);

        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(crossfade_seconds));
        self.load_current_item();

        if autoplay {
            let default_rate = self.default_rate();
            self.rate.store(default_rate, Ordering::Relaxed);
            let _ = self.send_to_slot(PlayerCmd::SetPaused(false));
            self.bus
                .publish(PlayerEvent::RateChanged { rate: default_rate });
            self.set_status(PlayerStatus::ReadyToPlay);
        } else {
            self.rate.store(0.0, Ordering::Relaxed);
            let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
            self.bus.publish(PlayerEvent::RateChanged { rate: 0.0 });
        }

        Ok(())
    }

    /// Insert a resource at the end and immediately crossfade to it.
    pub fn play_resource(&self, resource: Resource) -> Result<(), PlayError> {
        self.insert(resource, None);
        let index = self.item_count().saturating_sub(1);
        self.select_item(index, true)
    }

    /// Internal: set status and emit event if changed.
    fn set_status(&self, new_status: PlayerStatus) {
        let mut status = self.status.lock_sync();
        if *status != new_status {
            *status = new_status;
            drop(status);
            self.bus
                .publish(PlayerEvent::StatusChanged { status: new_status });
        }
    }

    /// Ensure the audio engine is started.
    fn ensure_engine_started(&self) -> Result<(), PlayError> {
        if !self.engine.is_running() {
            self.engine.start()?;
        }
        Ok(())
    }

    /// Ensure we have an active slot, allocating one if needed.
    fn ensure_slot(&self) -> Result<SlotId, PlayError> {
        let mut slot = self.current_slot.lock_sync();
        if let Some(id) = *slot {
            return Ok(id);
        }
        let id = self.engine.allocate_slot()?;
        *slot = Some(id);
        drop(slot);
        let effective = if self.is_muted() { 0.0 } else { self.volume() };
        self.engine.set_slot_volume(id, effective)?;
        Ok(id)
    }

    /// Apply effective volume to the current slot (per-instance).
    fn apply_effective_volume(&self, volume: f32) {
        if let Some(slot_id) = *self.current_slot.lock_sync() {
            debug!(volume, ?slot_id, "applying effective volume to slot");
            if let Err(e) = self.engine.set_slot_volume(slot_id, volume) {
                warn!(?e, volume, "failed to set slot volume");
            }
        } else {
            debug!(volume, "apply_effective_volume: no slot allocated yet");
        }
    }

    /// Send a command to the current slot's processor.
    fn send_to_slot(&self, cmd: PlayerCmd) -> Result<(), PlayError> {
        let slot_id = (*self.current_slot.lock_sync())
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        self.engine.send_slot_cmd(slot_id, cmd)
    }

    /// Load the item at `index` if it is the current item and a slot is active.
    ///
    /// Used by the FFI layer after a deferred (auto-load) insert completes.
    pub fn try_load_if_current(&self, index: usize) {
        if self.current_index() == index && self.current_slot.lock_sync().is_some() {
            self.load_current_item();
        }
    }

    /// Load the current queue item into the active slot.
    ///
    /// Takes the resource out of the queue (replacing with `None`), wraps it
    /// in [`PlayerResource`], and sends `LoadTrack` + `FadeIn` to the processor.
    fn load_current_item(&self) {
        let mut items = self.items.lock_sync();
        let index = self.current_index.load(Ordering::Relaxed);
        if index >= items.len() {
            return;
        }

        // Take the resource out of the queue (if not already consumed).
        let Some(resource) = items[index].take() else {
            return; // Already loaded
        };

        // Propagate current playback rate to the resource's audio pipeline.
        let current_rate = self.playback_rate_shared.load(Ordering::Relaxed);
        resource.set_playback_rate(current_rate);

        // Propagate host sample rate so the resampler is already initialised
        // with the correct ratio.  Without this, the resampler starts in
        // passthrough (host_sr = 0) and is recreated on the first worker
        // step when the real host rate becomes visible — the expensive
        // `make_sincs` call blocks the worker thread and starves other
        // tracks during crossfade.
        let host_sr = self.engine.master_sample_rate();
        if let Some(sr) = std::num::NonZeroU32::new(host_sr) {
            resource.set_host_sample_rate(sr);
        }

        let src = Arc::clone(resource.src());
        let player_resource = PlayerResource::new(resource, Arc::clone(&src), &self.pcm_pool);
        let arc_resource = Arc::new(Mutex::new(player_resource));
        drop(items);

        // Send LoadTrack and FadeIn to the processor.
        let _ = self.send_to_slot(PlayerCmd::LoadTrack {
            resource: arc_resource,
            src: Arc::clone(&src),
        });
        let _ = self.send_to_slot(PlayerCmd::Transition(TrackTransition::FadeIn(src)));
    }
}

impl crate::traits::dj::eq::Equalizer for PlayerImpl {
    fn band_count(&self) -> usize {
        self.eq_band_count()
    }

    fn gain(&self, band: usize) -> Option<f32> {
        self.eq_gain(band)
    }

    fn set_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        self.set_eq_gain(band, gain_db)
    }

    fn reset(&self) -> Result<(), PlayError> {
        self.reset_eq()
    }
}

#[cfg(test)]
mod tests {
    use kithara_audio::mock::TestPcmReader;
    use kithara_decode::PcmSpec;
    use kithara_events::Event;
    use kithara_test_utils::kithara;

    use super::*;

    #[derive(Clone, Copy)]
    enum PlayerBasicScenario {
        ActionAtItemEndDefault,
        AdvanceOnEmpty,
        EngineAccessor,
        QueueStartsEmpty,
        SendToSlotWithoutSlot,
        StartsPaused,
    }

    #[derive(Clone, Copy)]
    enum InsertScenario {
        AppendTwice,
        InsertAtPosition,
    }

    #[derive(Clone, Copy)]
    enum RemoveAtScenario {
        ExistingItem,
        OutOfBounds,
        ShiftCurrentIndex,
    }

    fn mock_spec() -> PcmSpec {
        PcmSpec {
            channels: 2,
            sample_rate: 44100,
        }
    }

    fn make_resource(duration_secs: f64) -> Resource {
        Resource::from_reader(TestPcmReader::new(mock_spec(), duration_secs))
    }

    #[kithara::test]
    #[case(PlayerBasicScenario::StartsPaused)]
    #[case(PlayerBasicScenario::QueueStartsEmpty)]
    #[case(PlayerBasicScenario::ActionAtItemEndDefault)]
    #[case(PlayerBasicScenario::AdvanceOnEmpty)]
    #[case(PlayerBasicScenario::EngineAccessor)]
    #[case(PlayerBasicScenario::SendToSlotWithoutSlot)]
    fn player_basic_behaviors(#[case] scenario: PlayerBasicScenario) {
        let player = PlayerImpl::new(PlayerConfig::default());
        match scenario {
            PlayerBasicScenario::StartsPaused => {
                assert!((player.rate() - 0.0).abs() < f32::EPSILON);
                assert_eq!(player.status(), PlayerStatus::Unknown);
            }
            PlayerBasicScenario::QueueStartsEmpty => {
                assert_eq!(player.item_count(), 0);
            }
            PlayerBasicScenario::ActionAtItemEndDefault => {
                assert_eq!(player.action_at_item_end(), ActionAtItemEnd::Advance);
            }
            PlayerBasicScenario::AdvanceOnEmpty => {
                player.advance_to_next_item();
                assert_eq!(player.current_index(), 0);
            }
            PlayerBasicScenario::EngineAccessor => {
                assert!(!player.engine().is_running());
            }
            PlayerBasicScenario::SendToSlotWithoutSlot => {
                let result = player.send_to_slot(PlayerCmd::SetPaused(true));
                assert!(result.is_err());
            }
        }
    }

    #[kithara::test]
    fn player_pause_sets_rate_zero() {
        let player = PlayerImpl::new(PlayerConfig::default());
        // Don't call play() (requires audio hardware); just test pause logic.
        player.rate.store(1.0, Ordering::Relaxed);
        player.pause();
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_volume_clamps() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_volume(2.0);
        assert!((player.volume() - 1.0).abs() < f32::EPSILON);
        player.set_volume(-1.0);
        assert!((player.volume() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_muted() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!(!player.is_muted());
        player.set_muted(true);
        assert!(player.is_muted());
    }

    #[kithara::test]
    fn player_session_ducking_roundtrip() {
        let _lock = crate::impls::engine::ducking_test_lock().lock_sync();
        let player = PlayerImpl::new(PlayerConfig::default());
        player
            .set_session_ducking(SessionDuckingMode::Soft)
            .unwrap();
        assert_eq!(player.session_ducking(), SessionDuckingMode::Soft);
        player.set_session_ducking(SessionDuckingMode::Off).unwrap();
    }

    #[kithara::test]
    fn player_crossfade_duration() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!((player.crossfade_duration() - 1.0).abs() < f32::EPSILON);
        player.set_crossfade_duration(3.0);
        assert!((player.crossfade_duration() - 3.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_events_subscribe() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let mut rx = player.subscribe();
        // Trigger an event without requiring audio hardware.
        player.set_volume(0.5);
        let event = rx.try_recv();
        assert!(event.is_ok());
    }

    #[kithara::test]
    fn player_config_custom() {
        let config = PlayerConfig {
            abr: None,
            bus: None,
            crossfade_duration: 2.0,
            default_rate: 0.5,
            eq_layout: generate_log_spaced_bands(5),
            max_slots: 2,
            pcm_pool: None,
        };
        let player = PlayerImpl::new(config);
        assert!((player.crossfade_duration() - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_config_builder() {
        let config = PlayerConfig::default()
            .with_max_slots(8)
            .with_default_rate(0.5)
            .with_crossfade_duration(2.5)
            .with_eq_layout(generate_log_spaced_bands(5));
        assert_eq!(config.max_slots, 8);
        assert!((config.default_rate - 0.5).abs() < f32::EPSILON);
        assert!((config.crossfade_duration - 2.5).abs() < f32::EPSILON);
        assert_eq!(config.eq_layout.len(), 5);
    }

    #[kithara::test(tokio)]
    #[case(InsertScenario::AppendTwice, 2)]
    #[case(InsertScenario::InsertAtPosition, 3)]
    async fn player_insert_scenarios(
        #[case] scenario: InsertScenario,
        #[case] expected_count: usize,
    ) {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        if matches!(scenario, InsertScenario::InsertAtPosition) {
            player.insert(make_resource(3.0), Some(0));
        }
        assert_eq!(player.item_count(), expected_count);
    }

    #[kithara::test(tokio)]
    #[case(RemoveAtScenario::ExistingItem)]
    #[case(RemoveAtScenario::OutOfBounds)]
    #[case(RemoveAtScenario::ShiftCurrentIndex)]
    async fn player_remove_at_scenarios(#[case] scenario: RemoveAtScenario) {
        let player = PlayerImpl::new(PlayerConfig::default());
        match scenario {
            RemoveAtScenario::ExistingItem => {
                player.insert(make_resource(1.0), None);
                player.insert(make_resource(2.0), None);
                let removed = player.remove_at(0);
                assert!(removed.is_some());
                assert_eq!(player.item_count(), 1);
            }
            RemoveAtScenario::OutOfBounds => {
                player.insert(make_resource(1.0), None);
                assert!(player.remove_at(5).is_none());
                assert_eq!(player.item_count(), 1);
            }
            RemoveAtScenario::ShiftCurrentIndex => {
                player.insert(make_resource(1.0), None);
                player.insert(make_resource(2.0), None);
                player.insert(make_resource(3.0), None);
                player.advance_to_next_item();
                player.advance_to_next_item();
                assert_eq!(player.current_index(), 2);
                player.remove_at(0);
                assert_eq!(player.current_index(), 1);
                assert_eq!(player.item_count(), 2);
            }
        }
    }

    #[kithara::test(tokio)]
    #[case(false)]
    #[case(true)]
    async fn player_remove_all_resets_state(#[case] with_resources: bool) {
        let player = PlayerImpl::new(PlayerConfig::default());
        if with_resources {
            player.insert(make_resource(1.0), None);
            player.insert(make_resource(2.0), None);
            player.insert(make_resource(3.0), None);
            assert_eq!(player.item_count(), 3);
        }
        player.remove_all_items();
        assert_eq!(player.item_count(), 0);
        assert_eq!(player.current_index(), 0);
        assert_eq!(player.status(), PlayerStatus::Unknown);
    }

    #[kithara::test(tokio)]
    async fn player_advance_through_queue() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        player.insert(make_resource(3.0), None);
        assert_eq!(player.current_index(), 0);
        player.advance_to_next_item();
        assert_eq!(player.current_index(), 1);
        player.advance_to_next_item();
        assert_eq!(player.current_index(), 2);
        // Cannot advance past last
        player.advance_to_next_item();
        assert_eq!(player.current_index(), 2);
    }

    #[kithara::test(tokio)]
    async fn player_advance_emits_event() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        let mut rx = player.subscribe();
        player.advance_to_next_item();
        let event = rx.try_recv();
        assert!(matches!(
            event,
            Ok(Event::Player(PlayerEvent::CurrentItemChanged))
        ));
    }

    #[kithara::test(tokio)]
    async fn player_play_without_audio_hardware_logs_warning() {
        // play() should not panic even when audio hardware is unavailable.
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        // This will attempt to start engine, fail gracefully, and return.
        player.play();
        // After a failed play, rate may or may not be set depending on
        // where the failure occurs. The key invariant: no panic.
    }

    #[kithara::test]
    fn player_default_rate_getter_setter() {
        let player = PlayerImpl::new(PlayerConfig::default());
        // Default rate from config is 1.0
        assert!((player.default_rate() - 1.0).abs() < f32::EPSILON);
        // Change at runtime
        player.set_default_rate(0.75);
        assert!((player.default_rate() - 0.75).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_play_uses_runtime_default_rate() {
        let player = PlayerImpl::new(PlayerConfig::default());
        // Override default rate before play
        player.set_default_rate(0.5);
        // play() stores the rate before attempting engine start (which fails
        // in test env without audio hardware).
        player.play();
        // Rate should reflect the runtime default_rate, not config's 1.0
        assert!((player.rate() - 0.5).abs() < f32::EPSILON);
    }

    #[kithara::test(tokio)]
    async fn player_multiple_events_in_order() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let mut rx = player.subscribe();

        player.set_volume(0.5);
        player.set_muted(true);
        player.set_rate(2.0);

        let e1 = rx.try_recv();
        let e2 = rx.try_recv();
        let e3 = rx.try_recv();
        assert!(matches!(
            e1,
            Ok(Event::Player(PlayerEvent::VolumeChanged { .. }))
        ));
        assert!(matches!(
            e2,
            Ok(Event::Player(PlayerEvent::MuteChanged { .. }))
        ));
        assert!(matches!(
            e3,
            Ok(Event::Player(PlayerEvent::RateChanged { .. }))
        ));
    }

    #[kithara::test(tokio)]
    async fn player_negative_crossfade_duration_clamped() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_crossfade_duration(-5.0);
        assert!((player.crossfade_duration() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn set_rate_updates_shared_atomic() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_rate(2.0);
        let shared = player.playback_rate_shared();
        assert!((shared.load(Ordering::Relaxed) - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn set_rate_clamps_invalid_values() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_rate(0.0);
        assert!(player.rate() >= 0.01);
        assert!(player.playback_rate_shared().load(Ordering::Relaxed) >= 0.01);

        player.set_rate(-1.0);
        assert!(player.rate() >= 0.01);
    }

    #[kithara::test]
    fn player_exposes_worker() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let _w = player.worker();
        // Worker should be accessible and clonable.
        let _w2 = player.worker().clone();
    }
}
