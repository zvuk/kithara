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
use kithara_decode::GaplessMode;
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
    impls::{
        player_notification::{PlayerNotification, TrackPlaybackStopReason},
        resource::Resource,
    },
    types::{PlayerStatus, SessionDuckingMode, SlotId},
};

/// Minimum playback rate to prevent stalling.
const MIN_PLAYBACK_RATE: f32 = 0.01;

struct QueuedResource {
    item_id: Option<Arc<str>>,
    resource: Resource,
}

/// Internal auto-advance state for the next queue item.
///
/// `PlayerImpl` remains the single orchestration owner for queue promotion:
/// the queue still owns `current_index`, while `PendingNext` only tracks the
/// already-enqueued successor and whether it has been activated.
struct PendingNext {
    index: usize,
    src: Arc<str>,
    activated: bool,
}

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
    /// Secondary lead time before EOF at which the next queued item is
    /// loaded into the processor; independent of crossfade.
    ///
    /// Effective preload trigger threshold =
    /// `max(prefetch_duration, crossfade_duration) + block_seconds`.
    /// The crossfade activation moment is unaffected. Set greater than
    /// `crossfade_duration` (especially when `crossfade_duration = 0`) so
    /// network probe + initial decode of the next track can finish before
    /// the audio thread runs out of PCM on the current track. Default: 3.5.
    #[derivative(Default(value = "3.5"))]
    pub prefetch_duration: f32,
    /// Default playback rate (1.0 = normal). Default: 1.0.
    #[derivative(Default(value = "1.0"))]
    pub default_rate: f32,
    /// EQ band layout. Default: 10-band log-spaced.
    #[derivative(Default(value = "generate_log_spaced_bands(10)"))]
    pub eq_layout: Vec<EqBandConfig>,
    /// How resources created for this player trim leading/trailing PCM.
    pub gapless_mode: GaplessMode,
    /// Maximum concurrent slots in the engine. Default: 4.
    #[derivative(Default(value = "4"))]
    pub max_slots: usize,
    /// PCM buffer pool for audio-thread scratch buffers.
    ///
    /// Propagated to the underlying [`EngineImpl`]. When `None`, the global
    /// PCM pool is used.
    pub pcm_pool: Option<PcmPool>,
    /// Built-in linear (`next = current + 1`) auto-advance handler that
    /// reacts to the audio-thread prefetch / handover triggers via
    /// [`PlayerImpl::arm_next`] and [`PlayerImpl::commit_next`].
    ///
    /// Default: `true`. Standalone callers (tests, demos) get gapless
    /// auto-advance for free. Higher-level orchestrators
    /// (`kithara_queue::Queue`) disable this and drive auto-advance
    /// themselves through the public arm / commit API.
    #[derivative(Default(value = "true"))]
    pub auto_advance_enabled: bool,
}

/// Concrete Player implementation managing items queue.
///
/// Owns an [`EngineImpl`] and sends commands to the active slot's processor.
/// When `play()` is called, the engine is lazily started and a slot is
/// allocated. The current queue item is taken out of the queue, wrapped in
/// [`PlayerResource`], and sent to the processor via `PlayerCmd::LoadTrack`.
pub struct PlayerImpl {
    config: PlayerConfig,

    bus: EventBus,
    crossfade_duration: AtomicF32,
    prefetch_duration: AtomicF32,
    current_index: AtomicUsize,
    current_slot: Mutex<Option<SlotId>>,
    default_rate: AtomicF32,
    /// Items drop before engine — Audio tracks unregister from worker
    /// while it is still alive.
    items: Mutex<Vec<Option<QueuedResource>>>,
    muted: AtomicBool,
    pcm_pool: PcmPool,
    /// Shared playback rate propagated to the audio pipeline resampler.
    playback_rate_shared: Arc<AtomicF32>,
    pending_next: Mutex<Option<PendingNext>>,
    auto_advance_enabled: AtomicBool,
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

        Self::new_with_engine(config, resolved_pool, bus, engine)
    }

    #[cfg(any(test, feature = "backend-offline"))]
    #[must_use]
    pub fn with_engine(mut config: PlayerConfig, engine: EngineImpl) -> Self {
        let resolved_pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| pcm_pool().clone());
        let bus = config.bus.clone().unwrap_or_default();
        if config.abr.is_none() {
            config.abr = Some(AbrController::new(AbrOptions::default()));
        }

        Self::new_with_engine(config, resolved_pool, bus, engine)
    }

    fn new_with_engine(
        config: PlayerConfig,
        resolved_pool: PcmPool,
        bus: EventBus,
        engine: EngineImpl,
    ) -> Self {
        Self {
            crossfade_duration: AtomicF32::new(config.crossfade_duration),
            prefetch_duration: AtomicF32::new(config.prefetch_duration.max(0.0)),
            current_index: AtomicUsize::new(0),
            current_slot: Mutex::new(None),
            default_rate: AtomicF32::new(config.default_rate),
            bus,
            items: Mutex::new(Vec::new()),
            muted: AtomicBool::new(false),
            pcm_pool: resolved_pool,
            playback_rate_shared: Arc::new(AtomicF32::new(config.default_rate)),
            pending_next: Mutex::new(None),
            auto_advance_enabled: AtomicBool::new(config.auto_advance_enabled),
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
        config.gapless_mode = self.config.gapless_mode;
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
        self.replace_item_tagged(index, resource, None);
    }

    /// Replace a consumed (or existing) resource at the given index with item identity metadata.
    pub fn replace_item_tagged(&self, index: usize, resource: Resource, item_id: Option<Arc<str>>) {
        let mut items = self.items.lock_sync();
        if index < items.len() {
            items[index] = Some(QueuedResource { item_id, resource });
            drop(items);
            debug!(index, "item replaced");
        }
    }

    /// Pre-allocate empty slots so `replace_item` can fill them by index.
    pub fn reserve_slots(&self, count: usize) {
        self.items.lock_sync().resize_with(count, || None);
        debug!(count, "slots reserved");
    }

    /// Read the underlying audio `src` of `items[index]` without taking
    /// it. Returns `None` when the slot is empty (loader hasn't filled
    /// it yet, or the engine has already taken it via `enqueue_to_processor`).
    /// Callers use this to capture the src that will be sent to the
    /// audio thread when they subsequently call `select_item_with_crossfade`.
    #[must_use]
    pub fn item_src(&self, index: usize) -> Option<Arc<str>> {
        self.items
            .lock_sync()
            .get(index)?
            .as_ref()
            .map(|q| Arc::clone(q.resource.src()))
    }

    /// Insert a resource with optional queue-item identity metadata at a specific position,
    /// or append to the end.
    pub fn insert(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        at_position: Option<usize>,
    ) {
        let mut items = self.items.lock_sync();
        let pos = at_position.map_or(items.len(), |i| i.min(items.len()));
        items.insert(pos, Some(QueuedResource { item_id, resource }));
        debug!(count = items.len(), pos, "item inserted");
    }

    /// Remove item at index. Returns the removed resource, or `None` if out of bounds
    /// or already consumed.
    pub fn remove_at(&self, index: usize) -> Option<Resource> {
        self.unarm_next_internal(Some(self.current_index.load(Ordering::Relaxed)));

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
        removed.map(|queued| queued.resource)
    }

    /// Remove all items from the queue.
    pub fn remove_all_items(&self) {
        self.unarm_next_internal(Some(self.current_index.load(Ordering::Relaxed)));
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
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(self.prefetch_duration()));
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

    /// Get ducking mode for this player's engine session.
    pub fn session_ducking(&self) -> SessionDuckingMode {
        EngineImpl::session_ducking()
    }

    /// Set ducking mode for this player's engine session.
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

    /// Set prefetch lead time in seconds.
    ///
    /// Canonical owner of this knob is `kithara_queue::Queue` — prefer
    /// `Queue::set_prefetch_duration` for queue-driven applications.
    /// Controls how early the next queued item is loaded into the processor
    /// before EOF. Independent of crossfade activation.
    pub fn set_prefetch_duration(&self, seconds: f32) {
        let clamped = seconds.max(0.0);
        self.prefetch_duration.store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(clamped));
    }

    /// Get prefetch lead time in seconds.
    pub fn prefetch_duration(&self) -> f32 {
        self.prefetch_duration.load(Ordering::Relaxed)
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
    /// only when a track finishes via natural EOF.
    pub fn process_notifications(&self) {
        let Some(slot_id) = *self.current_slot.lock_sync() else {
            tracing::trace!("process_notifications: no current slot");
            return;
        };
        let Some(state) = self.engine.slot_shared_state(slot_id) else {
            tracing::warn!(?slot_id, "process_notifications: slot has no shared state");
            return;
        };

        let mut notifications = Vec::new();
        while let Some(notification) = state.notification_rx.lock_sync().try_pop() {
            notifications.push(notification);
        }

        if !notifications.is_empty() {
            tracing::debug!(count = notifications.len(), "process_notifications draining");
        }
        for notification in notifications {
            tracing::debug!(?notification, "process_notifications: handle");
            match notification.clone() {
                PlayerNotification::TrackRequested(_) => {
                    self.bus.publish(PlayerEvent::PrefetchRequested);
                    if self.auto_advance_enabled() {
                        let next_index = self.current_index.load(Ordering::Relaxed) + 1;
                        if next_index < self.item_count() {
                            let _ = self.arm_next(next_index);
                        }
                    }
                }
                PlayerNotification::TrackHandoverRequested(_) => {
                    if self.crossfade_duration() <= 0.0 {
                        // cf=0: arena handover at EOF does it; main thread
                        // finalises bookkeeping in the EOF branch below.
                        continue;
                    }
                    self.bus.publish(PlayerEvent::HandoverRequested);
                    if self.auto_advance_enabled()
                        && let Some(idx) = self.armed_next()
                    {
                        let _ = self.commit_next(idx);
                    }
                }
                PlayerNotification::TrackPlaybackStopped {
                    reason: TrackPlaybackStopReason::Eof,
                    ..
                } => {
                    self.handle_track_playback_stopped(notification);
                }
                _ => {
                    if let Some(event) = player_event_from_notification(notification.clone()) {
                        self.bus.publish(event);
                    } else {
                        tracing::trace!(?notification, "unhandled player notification");
                    }
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

        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(crossfade_seconds));
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(self.prefetch_duration()));

        // `arm_next` already took `items[index]`; reloading would be a
        // silent no-op. Promote the armed slot instead.
        let armed_for_index = self
            .pending_next
            .lock_sync()
            .as_ref()
            .is_some_and(|p| !p.activated && p.index == index);
        if armed_for_index {
            self.commit_next(index)?;
        } else {
            self.unarm_next_internal(Some(index));
            self.current_index.store(index, Ordering::Relaxed);
            self.bus.publish(PlayerEvent::CurrentItemChanged);
            self.load_current_item();
        }

        self.apply_autoplay(autoplay);
        Ok(())
    }

    fn apply_autoplay(&self, autoplay: bool) {
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
    }

    /// Insert a resource at the end and immediately crossfade to it.
    pub fn play_resource(&self, resource: Resource) -> Result<(), PlayError> {
        self.insert(resource, None, None);
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

    /// Whether the built-in linear auto-advance handler is enabled.
    #[must_use]
    pub fn auto_advance_enabled(&self) -> bool {
        self.auto_advance_enabled.load(Ordering::Relaxed)
    }

    /// Enable or disable the built-in linear auto-advance handler.
    ///
    /// External orchestrators (e.g. `kithara_queue::Queue`) disable this
    /// and drive auto-advance themselves through the public arm / commit
    /// API. Toggling this at runtime takes effect on the next audio-thread
    /// trigger.
    pub fn set_auto_advance_enabled(&self, enabled: bool) {
        self.auto_advance_enabled
            .store(enabled, Ordering::Relaxed);
    }

    /// Load `items[index]` into the audio-thread arena in `Preloading`
    /// state, ready for sample-accurate gapless stitch (cf=0) or parallel
    /// fade (cf>0).
    ///
    /// If a different next is already armed, it is unloaded first.
    /// Idempotent for the same index. Returns `Some(src)` on success;
    /// `None` if `items[index]` is empty (loader hasn't filled it yet) or
    /// `index` is out of range.
    pub fn arm_next(&self, index: usize) -> Option<Arc<str>> {
        let current_index = self.current_index.load(Ordering::Relaxed);
        if index >= self.item_count() {
            return None;
        }

        let mut pending_lock = self.pending_next.lock_sync();
        if let Some(existing) = pending_lock.as_ref() {
            if existing.index == index {
                return Some(Arc::clone(&existing.src));
            }
            if !(existing.activated && existing.index == current_index) {
                let prev_src = Arc::clone(&existing.src);
                drop(pending_lock);
                let _ = self.send_to_slot(PlayerCmd::UnloadTrack { src: prev_src });
                pending_lock = self.pending_next.lock_sync();
            }
            *pending_lock = None;
        }
        drop(pending_lock);

        let src = self.enqueue_to_processor(index)?;
        *self.pending_next.lock_sync() = Some(PendingNext {
            index,
            src: Arc::clone(&src),
            activated: false,
        });
        Some(src)
    }

    /// Commit the previously armed next track and start the cross-fade.
    ///
    /// Sends `FadeIn` to the audio thread for the armed slot, updates
    /// `current_index`, and publishes `CurrentItemChanged`.
    ///
    /// # Errors
    /// - [`PlayError::NotReady`] if no slot is armed.
    /// - [`PlayError::Internal`] if `index` does not match
    ///   [`Self::armed_next`].
    pub fn commit_next(&self, index: usize) -> Result<(), PlayError> {
        let mut pending_lock = self.pending_next.lock_sync();
        let pending = pending_lock.as_mut().ok_or(PlayError::NotReady)?;
        if pending.index != index {
            return Err(PlayError::Internal(format!(
                "commit_next index mismatch: requested {index}, armed {}",
                pending.index
            )));
        }
        if pending.activated {
            return Ok(());
        }
        let src = Arc::clone(&pending.src);
        pending.activated = true;
        drop(pending_lock);

        self.start_playback(src);
        let current_index = self.current_index.load(Ordering::Relaxed);
        if index != current_index {
            self.current_index.store(index, Ordering::Relaxed);
            self.bus.publish(PlayerEvent::CurrentItemChanged);
        }
        Ok(())
    }

    /// Drop the armed next slot without committing.
    ///
    /// Sends `UnloadTrack` to the audio thread for the armed src and
    /// clears `pending_next`. Skips the unload if the armed slot has
    /// already been activated for the current index (the activated track
    /// is now the leading one — unloading would silence playback).
    pub fn unarm_next(&self) {
        self.unarm_next_internal(Some(self.current_index.load(Ordering::Relaxed)));
    }

    /// Snapshot of the armed-next index. `None` when no slot is armed
    /// (or after `commit_next` has consumed it for the current handover).
    #[must_use]
    pub fn armed_next(&self) -> Option<usize> {
        self.pending_next
            .lock_sync()
            .as_ref()
            .filter(|pending| !pending.activated)
            .map(|pending| pending.index)
    }

    fn unarm_next_internal(&self, current_index_hint: Option<usize>) {
        let pending = self.pending_next.lock_sync().take();
        let Some(pending) = pending else {
            return;
        };
        let preserve_active_current = current_index_hint
            .is_some_and(|index| pending.activated && pending.index == index);
        if !preserve_active_current {
            let _ = self.send_to_slot(PlayerCmd::UnloadTrack { src: pending.src });
        }
    }

    fn handle_track_playback_stopped(&self, notification: PlayerNotification) {
        if let Some(event) = player_event_from_notification(notification) {
            self.bus.publish(event);
        }

        self.finalize_handover_if_armed();
    }

    /// Promote the armed slot to current after an audio-thread handover.
    ///
    /// Called on `TrackPlaybackStopped { Eof }` for the outgoing track.
    /// Two paths:
    /// - `pending.activated == true`: `commit_next` already moved the
    ///   bookkeeping at the handover threshold (cf>0). Just clear.
    /// - `pending.activated == false`: the audio thread performed the
    ///   in-block arena handover (cf=0). Sync `current_index` and emit
    ///   `CurrentItemChanged`; do **not** dispatch a fresh `FadeIn` —
    ///   the arena promotion already advanced the track to `Playing`.
    fn finalize_handover_if_armed(&self) {
        let pending = self.pending_next.lock_sync().take();
        let Some(pending) = pending else {
            return;
        };

        if pending.activated {
            return;
        }

        let current_index = self.current_index.load(Ordering::Relaxed);
        if pending.index == current_index || pending.index >= self.item_count() {
            return;
        }
        self.current_index.store(pending.index, Ordering::Relaxed);
        self.bus.publish(PlayerEvent::CurrentItemChanged);
    }

    /// Load the current queue item into the active slot.
    ///
    /// Takes the resource out of the queue (replacing with `None`), wraps it
    /// in [`PlayerResource`], and sends `LoadTrack` + `FadeIn` to the processor.
    fn load_current_item(&self) {
        let index = self.current_index.load(Ordering::Relaxed);
        if let Some(src) = self.enqueue_to_processor(index) {
            self.start_playback(src);
        }
    }

    fn enqueue_to_processor(&self, index: usize) -> Option<Arc<str>> {
        let mut items = self.items.lock_sync();
        if index >= items.len() {
            return None;
        }

        let queued = items[index].take()?;
        let QueuedResource { item_id, resource } = queued;

        let current_rate = self.playback_rate_shared.load(Ordering::Relaxed);
        resource.set_playback_rate(current_rate);

        let host_sr = self.engine.master_sample_rate();
        if let Some(sr) = std::num::NonZeroU32::new(host_sr) {
            resource.set_host_sample_rate(sr);
        }

        let src = Arc::clone(resource.src());
        let player_resource = PlayerResource::new(resource, Arc::clone(&src), &self.pcm_pool);
        let arc_resource = Arc::new(Mutex::new(player_resource));
        drop(items);

        let _ = self.send_to_slot(PlayerCmd::LoadTrack {
            resource: arc_resource,
            item_id,
            src: Arc::clone(&src),
        });

        Some(src)
    }

    fn start_playback(&self, src: Arc<str>) {
        let _ = self.send_to_slot(PlayerCmd::Transition(TrackTransition::FadeIn(src)));
    }
}

fn player_event_from_notification(notification: PlayerNotification) -> Option<PlayerEvent> {
    match notification {
        PlayerNotification::TrackPlaybackStopped {
            reason: TrackPlaybackStopReason::Eof,
            src,
            item_id,
        } => Some(PlayerEvent::ItemDidPlayToEnd { src, item_id }),
        _ => None,
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
    use std::sync::Arc;

    use kithara_audio::mock::TestPcmReader;
    use kithara_decode::PcmSpec;
    use kithara_events::Event;
    use kithara_test_utils::kithara;

    use super::*;

    #[derive(Clone, Copy)]
    enum PlayerBasicScenario {
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
        Resource::from_reader_with_src(
            TestPcmReader::new(mock_spec(), duration_secs),
            Arc::from(format!("test-resource-{duration_secs}")),
        )
    }

    #[kithara::test]
    fn prepare_config_applies_player_gapless_mode() {
        let player = PlayerImpl::new(PlayerConfig {
            gapless_mode: GaplessMode::Disabled,
            ..PlayerConfig::default()
        });
        let mut config = crate::impls::config::ResourceConfig::new("https://example.com/song.mp3")
            .expect("valid resource config");

        player.prepare_config(&mut config);

        assert_eq!(config.gapless_mode, GaplessMode::Disabled);
        player.worker().shutdown();
    }

    #[kithara::test]
    #[case(PlayerBasicScenario::StartsPaused)]
    #[case(PlayerBasicScenario::QueueStartsEmpty)]
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
    fn player_prefetch_duration() {
        let player = PlayerImpl::new(PlayerConfig::default());
        // Default from PlayerConfig.
        assert!((player.prefetch_duration() - 3.5).abs() < f32::EPSILON);
        player.set_prefetch_duration(8.0);
        assert!((player.prefetch_duration() - 8.0).abs() < f32::EPSILON);
        // Negative values clamp to zero.
        player.set_prefetch_duration(-1.0);
        assert!((player.prefetch_duration() - 0.0).abs() < f32::EPSILON);
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
            prefetch_duration: 5.0,
            default_rate: 0.5,
            eq_layout: generate_log_spaced_bands(5),
            gapless_mode: GaplessMode::MediaOnly,
            max_slots: 2,
            pcm_pool: None,
            auto_advance_enabled: true,
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
            .with_prefetch_duration(7.0)
            .with_eq_layout(generate_log_spaced_bands(5));
        assert_eq!(config.max_slots, 8);
        assert!((config.default_rate - 0.5).abs() < f32::EPSILON);
        assert!((config.crossfade_duration - 2.5).abs() < f32::EPSILON);
        assert!((config.prefetch_duration - 7.0).abs() < f32::EPSILON);
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
        player.insert(make_resource(1.0), None, None);
        player.insert(make_resource(2.0), None, None);
        if matches!(scenario, InsertScenario::InsertAtPosition) {
            player.insert(make_resource(3.0), None, Some(0));
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
                player.insert(make_resource(1.0), None, None);
                player.insert(make_resource(2.0), None, None);
                let removed = player.remove_at(0);
                assert!(removed.is_some());
                assert_eq!(player.item_count(), 1);
            }
            RemoveAtScenario::OutOfBounds => {
                player.insert(make_resource(1.0), None, None);
                assert!(player.remove_at(5).is_none());
                assert_eq!(player.item_count(), 1);
            }
            RemoveAtScenario::ShiftCurrentIndex => {
                player.insert(make_resource(1.0), None, None);
                player.insert(make_resource(2.0), None, None);
                player.insert(make_resource(3.0), None, None);
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
            player.insert(make_resource(1.0), None, None);
            player.insert(make_resource(2.0), None, None);
            player.insert(make_resource(3.0), None, None);
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
        player.insert(make_resource(1.0), None, None);
        player.insert(make_resource(2.0), None, None);
        player.insert(make_resource(3.0), None, None);
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
        player.insert(make_resource(1.0), None, None);
        player.insert(make_resource(2.0), None, None);
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
        player.insert(make_resource(1.0), None, None);
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

    #[kithara::test]
    fn eof_playback_stopped_notification_maps_to_item_end_event() {
        let event = player_event_from_notification(PlayerNotification::TrackPlaybackStopped {
            src: Arc::from("track.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Eof,
        });
        assert!(matches!(event, Some(PlayerEvent::ItemDidPlayToEnd { .. })));
    }

    #[kithara::test]
    fn playback_stopped_notification_does_not_map_to_item_end_event() {
        let event = player_event_from_notification(PlayerNotification::TrackPlaybackStopped {
            src: Arc::from("track.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Stop,
        });
        assert!(event.is_none());
    }

    fn make_tagged_resource(item_id: &'static str, duration_secs: f64) -> (Resource, Arc<str>) {
        let item_id = Arc::<str>::from(item_id);
        (
            Resource::from_reader_with_src(
                TestPcmReader::new(mock_spec(), duration_secs),
                Arc::from(format!("memory://{item_id}")),
            ),
            item_id,
        )
    }

    /// Build a `PlayerImpl` wired to a per-instance offline session
    /// whose firewheel graph is stepped synchronously via the returned
    /// `OfflineSessionHandle`. Each test gets a private session worker
    /// thread, so multiple gapless tests can run in parallel without
    /// contending on global state.
    fn make_offline_player(
        crossfade_duration: f32,
    ) -> (PlayerImpl, crate::impls::engine::OfflineSessionHandle) {
        use kithara_events::EventBus;
        let bus = EventBus::default();
        let engine_config = EngineConfig {
            sample_rate: 44_100,
            ..EngineConfig::default()
        };
        let (engine, session) = EngineImpl::new_offline(engine_config, bus.clone());
        let player_config = PlayerConfig {
            bus: Some(bus),
            crossfade_duration,
            ..PlayerConfig::default()
        };
        let player = PlayerImpl::with_engine(player_config, engine);
        (player, session)
    }

    /// Drain the bus subscriber after pumping the audio-thread
    /// notification queue. Mirrors the production tick path. Tolerates
    /// `Lagged` errors from the broadcast channel by continuing past
    /// them — the test's contract is "events fired *eventually*", not
    /// "no events ever dropped".
    fn drain_player_events(
        player: &PlayerImpl,
        rx: &mut kithara_events::EventReceiver,
    ) -> Vec<PlayerEvent> {
        use kithara_platform::tokio::sync::broadcast::error::TryRecvError;
        player.process_notifications();
        let mut events = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(Event::Player(event)) => events.push(event),
                Ok(_) => continue,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        events
    }

    /// Step the offline graph block-by-block, draining events after
    /// every block, until both predicates are satisfied or the budget
    /// runs out. Returns the accumulated event log.
    fn render_until_events(
        player: &PlayerImpl,
        session: &crate::impls::engine::OfflineSessionHandle,
        rx: &mut kithara_events::EventReceiver,
        max_blocks: usize,
        mut done: impl FnMut(&[PlayerEvent]) -> bool,
    ) -> Vec<PlayerEvent> {
        const BLOCK_FRAMES: usize = 512;
        let mut events = Vec::new();
        for _ in 0..max_blocks {
            let _ = session.render(BLOCK_FRAMES);
            events.extend(drain_player_events(player, rx));
            if done(&events) {
                break;
            }
        }
        events
    }

    #[kithara::test]
    fn queue_auto_advance_cf_zero_emits_terminal_before_current_changed() {
        let (player, session) = make_offline_player(0.0);
        let mut rx = player.subscribe();
        let (first, first_id) = make_tagged_resource("item-1", 0.05);
        let (second, _) = make_tagged_resource("item-2", 0.05);
        player.insert(first, Some(Arc::clone(&first_id)), None);
        player.insert(second, Some(Arc::from("item-2")), None);

        player.play();
        let _ = drain_player_events(&player, &mut rx);

        let events = render_until_events(&player, &session, &mut rx, 256, |evs| {
            evs.iter().any(|e| {
                matches!(
                    e,
                    PlayerEvent::ItemDidPlayToEnd { .. }
                )
            }) && evs.iter().filter(|e| matches!(e, PlayerEvent::CurrentItemChanged)).count() >= 1
        });

        let item_end = events.iter().position(|e| {
            matches!(
                e,
                PlayerEvent::ItemDidPlayToEnd { .. }
            )
        });
        // Take the CurrentItemChanged that fires *after* the terminal
        // event — the first one on the timeline is the play() emission.
        let handover_changed = item_end.and_then(|end_idx| {
            events
                .iter()
                .enumerate()
                .skip(end_idx + 1)
                .find(|(_, e)| matches!(e, PlayerEvent::CurrentItemChanged))
                .map(|(i, _)| i)
        });

        assert!(item_end.is_some(), "no terminal event: {events:?}");
        assert!(
            handover_changed.is_some(),
            "no post-terminal CurrentItemChanged: {events:?}"
        );
        assert!(item_end.unwrap() < handover_changed.unwrap());
        assert_eq!(player.current_index(), 1);
    }

    #[kithara::test]
    fn queue_auto_advance_cf_one_activates_before_first_terminal_event() {
        let (player, session) = make_offline_player(1.0);
        let mut rx = player.subscribe();
        // Tracks must outlast the crossfade window so the audio thread
        // has room to enter prefetch+handover before EOF; 0.05 s would
        // race the decoder warm-up in the offline session.
        let (first, _) = make_tagged_resource("item-1", 1.5);
        let (second, _) = make_tagged_resource("item-2", 1.5);
        player.insert(first, Some(Arc::from("item-1")), None);
        player.insert(second, Some(Arc::from("item-2")), None);

        player.play();
        // Drop the play()-side initial events (Status/CurrentItem/Rate).
        let _ = drain_player_events(&player, &mut rx);

        let events = render_until_events(&player, &session, &mut rx, 512, |evs| {
            evs.iter().any(|e| {
                matches!(
                    e,
                    PlayerEvent::ItemDidPlayToEnd { .. }
                )
            })
        });

        // Crossfade window: handover CurrentItemChanged must precede
        // the outgoing track's terminal event so the two tracks
        // overlap in render time.
        let handover_changed = events
            .iter()
            .position(|e| matches!(e, PlayerEvent::CurrentItemChanged));
        let item_end = events.iter().position(|e| {
            matches!(
                e,
                PlayerEvent::ItemDidPlayToEnd { .. }
            )
        });

        assert!(handover_changed.is_some(), "no handover event: {events:?}");
        assert!(item_end.is_some(), "no terminal event: {events:?}");
        assert!(handover_changed.unwrap() < item_end.unwrap());
        assert_eq!(player.current_index(), 1);
    }

    #[kithara::test]
    fn queue_auto_advance_cf_ge_prefetch_still_advances() {
        // Regression: with `crossfade_duration >= prefetch_duration`, the
        // two trigger windows coincide. The audio thread used to suppress
        // `TrackRequested` in that case, leaving the consumer without an
        // armed slot; the subsequent `TrackHandoverRequested` then became
        // a no-op and the player got stuck on the first track. Default
        // demo config (cf=5, prefetch=3.5) hits this path.
        let (player, session) = make_offline_player(4.0);
        // Default prefetch_duration is 3.5 — strictly less than crossfade,
        // so prefetch_threshold == fade_threshold and the bug would
        // suppress `PrefetchRequested`.
        let mut rx = player.subscribe();
        let (first, _) = make_tagged_resource("item-1", 5.0);
        let (second, _) = make_tagged_resource("item-2", 5.0);
        player.insert(first, Some(Arc::from("item-1")), None);
        player.insert(second, Some(Arc::from("item-2")), None);

        player.play();
        let _ = drain_player_events(&player, &mut rx);

        let events = render_until_events(&player, &session, &mut rx, 1024, |evs| {
            evs.iter().any(|e| matches!(e, PlayerEvent::ItemDidPlayToEnd { .. }))
        });

        assert!(
            events
                .iter()
                .any(|e| matches!(e, PlayerEvent::PrefetchRequested)),
            "PrefetchRequested must fire when cf >= prefetch (windows coincide); got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PlayerEvent::HandoverRequested)),
            "HandoverRequested must fire; got {events:?}"
        );
        assert_eq!(
            player.current_index(),
            1,
            "auto-advance must reach the second item: {events:?}"
        );
    }

    #[kithara::test]
    fn auto_advance_enabled_default_and_toggle() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!(player.auto_advance_enabled(), "default must be on");
        player.set_auto_advance_enabled(false);
        assert!(!player.auto_advance_enabled());
        player.set_auto_advance_enabled(true);
        assert!(player.auto_advance_enabled());
    }

    #[kithara::test]
    fn auto_advance_disabled_via_config() {
        let player = PlayerImpl::new(PlayerConfig {
            auto_advance_enabled: false,
            ..PlayerConfig::default()
        });
        assert!(!player.auto_advance_enabled());
    }

    #[kithara::test]
    fn arm_next_loads_item_and_returns_src() {
        let (player, session) = make_offline_player(0.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("item-1", 0.05);
        let (second, _) = make_tagged_resource("item-2", 0.05);
        player.insert(first, Some(Arc::from("item-1")), None);
        player.insert(second, Some(Arc::from("item-2")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();

        let src = player.arm_next(1).expect("arm_next succeeds for items[1]");
        assert_eq!(player.armed_next(), Some(1));
        let _ = session.render(512);
        let notifications = player.drain_notifications();
        assert!(
            notifications.iter().any(|n| {
                n.contains("TrackLoaded") && n.contains(src.as_ref())
            }),
            "TrackLoaded must reach the audio thread; got {notifications:?}"
        );
    }

    #[kithara::test]
    fn arm_next_returns_none_for_empty_slot() {
        let (player, _session) = make_offline_player(0.0);
        player.set_auto_advance_enabled(false);
        player.reserve_slots(2);
        // Only items[0] populated.
        let (first, _) = make_tagged_resource("item-1", 0.05);
        player.replace_item_tagged(0, first, Some(Arc::from("item-1")));
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();

        assert!(player.arm_next(1).is_none(), "empty slot must yield None");
        assert_eq!(player.armed_next(), None);
    }

    #[kithara::test]
    fn arm_next_idempotent_for_same_index() {
        let (player, _session) = make_offline_player(0.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("item-1", 0.05);
        let (second, _) = make_tagged_resource("item-2", 0.05);
        player.insert(first, Some(Arc::from("item-1")), None);
        player.insert(second, Some(Arc::from("item-2")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();

        let first_src = player.arm_next(1).unwrap();
        let second_src = player.arm_next(1).unwrap();
        assert_eq!(first_src.as_ref(), second_src.as_ref());
        assert_eq!(player.armed_next(), Some(1));
    }

    #[kithara::test]
    fn arm_next_replaces_previously_armed_slot() {
        let (player, session) = make_offline_player(0.0);
        player.set_auto_advance_enabled(false);
        let (a, _) = make_tagged_resource("a", 0.05);
        let (b, _) = make_tagged_resource("b", 0.05);
        let (c, _) = make_tagged_resource("c", 0.05);
        player.insert(a, Some(Arc::from("a")), None);
        player.insert(b, Some(Arc::from("b")), None);
        player.insert(c, Some(Arc::from("c")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();

        let first = player.arm_next(1).unwrap();
        let _ = session.render(512);
        let _ = player.drain_notifications();
        let second = player.arm_next(2).unwrap();
        assert_ne!(first.as_ref(), second.as_ref());
        assert_eq!(player.armed_next(), Some(2));

        let _ = session.render(512);
        let notifications = player.drain_notifications();
        assert!(
            notifications.iter().any(|n| {
                n.contains("TrackUnloaded") && n.contains(first.as_ref())
            }),
            "previous arm must be unloaded; got {notifications:?}"
        );
    }

    #[kithara::test]
    fn commit_next_without_arm_returns_not_ready() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let err = player.commit_next(1).expect_err("must error");
        assert!(matches!(err, PlayError::NotReady));
    }

    #[kithara::test]
    fn commit_next_index_mismatch_returns_internal() {
        let (player, _session) = make_offline_player(1.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("a", 0.05);
        let (second, _) = make_tagged_resource("b", 0.05);
        player.insert(first, Some(Arc::from("a")), None);
        player.insert(second, Some(Arc::from("b")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();
        player.arm_next(1).unwrap();

        let err = player.commit_next(2).expect_err("mismatch");
        assert!(matches!(err, PlayError::Internal(_)));
    }

    #[kithara::test]
    fn commit_next_advances_index_and_publishes_event() {
        let (player, _session) = make_offline_player(1.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("a", 0.05);
        let (second, _) = make_tagged_resource("b", 0.05);
        player.insert(first, Some(Arc::from("a")), None);
        player.insert(second, Some(Arc::from("b")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();
        player.arm_next(1).unwrap();
        let mut rx = player.subscribe();

        player.commit_next(1).unwrap();
        assert_eq!(player.current_index(), 1);
        assert_eq!(player.armed_next(), None, "armed clears after commit");

        let mut saw_changed = false;
        for _ in 0..8 {
            match rx.try_recv() {
                Ok(Event::Player(PlayerEvent::CurrentItemChanged)) => saw_changed = true,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert!(saw_changed, "commit_next must publish CurrentItemChanged");
    }

    #[kithara::test]
    fn commit_next_idempotent_when_already_activated() {
        let (player, _session) = make_offline_player(1.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("a", 0.05);
        let (second, _) = make_tagged_resource("b", 0.05);
        player.insert(first, Some(Arc::from("a")), None);
        player.insert(second, Some(Arc::from("b")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();
        player.arm_next(1).unwrap();

        player.commit_next(1).unwrap();
        // Second call must not error or move state.
        player.commit_next(1).unwrap();
        assert_eq!(player.current_index(), 1);
    }

    #[kithara::test]
    fn unarm_next_clears_when_not_activated_and_unloads() {
        let (player, session) = make_offline_player(0.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("a", 0.05);
        let (second, _) = make_tagged_resource("b", 0.05);
        player.insert(first, Some(Arc::from("a")), None);
        player.insert(second, Some(Arc::from("b")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();
        let src = player.arm_next(1).unwrap();
        let _ = session.render(512);
        let _ = player.drain_notifications();

        player.unarm_next();
        assert_eq!(player.armed_next(), None);
        let _ = session.render(512);
        let notifications = player.drain_notifications();
        assert!(
            notifications.iter().any(|n| {
                n.contains("TrackUnloaded") && n.contains(src.as_ref())
            }),
            "unarm must send UnloadTrack; got {notifications:?}"
        );
    }

    #[kithara::test]
    fn unarm_next_preserves_activated_current() {
        let (player, _session) = make_offline_player(1.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("a", 0.05);
        let (second, _) = make_tagged_resource("b", 0.05);
        player.insert(first, Some(Arc::from("a")), None);
        player.insert(second, Some(Arc::from("b")), None);
        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();
        player.arm_next(1).unwrap();
        player.commit_next(1).unwrap();
        // Now pending_next has activated=true, index=1=current.
        // unarm_next must clear bookkeeping but NOT unload the leading
        // track (which is now playing).
        player.unarm_next();
        assert_eq!(player.armed_next(), None);
        assert_eq!(player.current_index(), 1);
    }

    #[kithara::test]
    fn select_item_clears_pending_next_and_unloads_preloaded_track() {
        let (player, session) = make_offline_player(1.0);
        // Disable the built-in auto-advancer so this test exercises the
        // explicit arm_next / select_item handshake without races
        // against the prefetch trigger.
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("item-1", 0.05);
        let (second, _) = make_tagged_resource("item-2", 0.05);
        let (third, _) = make_tagged_resource("item-3", 0.05);
        player.insert(first, Some(Arc::from("item-1")), None);
        player.insert(second, Some(Arc::from("item-2")), None);
        player.insert(third, Some(Arc::from("item-3")), None);

        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();
        let src = player.arm_next(1).expect("arm_next loads items[1]");
        assert_eq!(player.armed_next(), Some(1));

        player.select_item(2, true).unwrap();
        let _ = session.render(512);
        let notifications = player.drain_notifications();

        assert_eq!(player.armed_next(), None, "select_item must unarm");
        assert!(
            notifications.iter().any(|notification| {
                notification.contains("TrackUnloaded") && notification.contains(src.as_ref())
            }),
            "expected TrackUnloaded for preloaded src; notifications={notifications:?}"
        );
        assert_eq!(player.current_index(), 2);
    }

    /// Selecting the index that's already armed must promote the armed
    /// slot, not unload-then-reload. Without this, the second user-driven
    /// switch silently no-ops because `items[index]` was emptied by
    /// `arm_next`'s `take()` and `enqueue_to_processor` returns `None`.
    #[kithara::test]
    fn select_item_on_armed_index_promotes_armed_slot() {
        let (player, session) = make_offline_player(1.0);
        player.set_auto_advance_enabled(false);
        let (first, _) = make_tagged_resource("item-1", 0.05);
        let (second, _) = make_tagged_resource("item-2", 0.05);
        player.insert(first, Some(Arc::from("item-1")), None);
        player.insert(second, Some(Arc::from("item-2")), None);

        player.ensure_engine_started().unwrap();
        player.ensure_slot().unwrap();
        player.select_item(0, true).unwrap();
        let _ = session.render(256);
        let armed_src = player.arm_next(1).expect("arm_next loads items[1]");
        assert_eq!(player.armed_next(), Some(1));

        player.select_item(1, true).unwrap();
        let _ = session.render(256);
        let notifications = player.drain_notifications();

        assert_eq!(player.current_index(), 1);
        assert_eq!(player.armed_next(), None, "armed slot consumed by select");
        assert!(
            notifications.iter().any(|n| {
                n.contains("TrackChanged") && n.contains(armed_src.as_ref())
            }),
            "armed src must become the leading track; notifications={notifications:?}"
        );
        assert!(
            !notifications.iter().any(|n| {
                n.contains("TrackUnloaded") && n.contains(armed_src.as_ref())
            }),
            "armed src must NOT be unloaded; notifications={notifications:?}"
        );
    }
}
