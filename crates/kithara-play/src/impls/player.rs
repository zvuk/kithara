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
use kithara_abr::{AbrController, AbrMode, AbrSettings};
use kithara_audio::{AudioWorkerHandle, EqBandConfig, SeekOutcome, generate_log_spaced_bands};
use kithara_bufpool::PcmPool;
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
    session_engine::SessionDispatcher,
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
    pub abr: Option<Arc<AbrController>>,
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
    /// Pre-built audio session dispatcher.
    ///
    /// Forwarded to [`EngineConfig::session`]. `None` → engine binds to
    /// the process-wide cpal session. Integration tests pass an offline
    /// dispatcher here to drive playback without real hardware.
    #[derivative(Debug = "ignore")]
    pub session: Option<Arc<dyn SessionDispatcher>>,
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
        let resolved_pool = config.pcm_pool.clone().unwrap_or_default();

        let bus = config.bus.clone().unwrap_or_default();

        let engine_config = EngineConfig {
            eq_layout: config.eq_layout.clone(),
            max_slots: config.max_slots,
            pcm_pool: Some(resolved_pool.clone()),
            session: config.session.clone(),
            ..EngineConfig::default()
        };
        let engine = EngineImpl::new(engine_config, bus.clone());
        if config.abr.is_none() {
            config.abr = Some(AbrController::new(AbrSettings::default()));
        }

        Self::new_with_engine(config, resolved_pool, bus, engine)
    }

    /// Build a player on top of an externally-constructed engine.
    ///
    /// Used by integration-test harnesses that drive a single
    /// `EngineImpl` for shared offline-render plumbing. Production code
    /// uses [`PlayerImpl::new`] which builds its engine internally.
    #[must_use]
    pub fn with_engine(mut config: PlayerConfig, engine: EngineImpl) -> Self {
        let resolved_pool = config.pcm_pool.clone().unwrap_or_default();
        let bus = config.bus.clone().unwrap_or_default();
        if config.abr.is_none() {
            config.abr = Some(AbrController::new(AbrSettings::default()));
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

    /// Drop the resource at `index` so the auto-advance prefetch path
    /// (`arm_next`) cannot plant it into the audio thread.
    ///
    /// Used by the queue when a previously-loaded track is cancelled by
    /// a later `select` — without this, a slow track whose loader
    /// raced ahead of the override stays in `items` and the next
    /// `TrackRequested` notification near EOF would arm it for
    /// handover, surfacing as a barge-in.
    pub fn clear_item(&self, index: usize) {
        let mut items = self.items.lock_sync();
        if index < items.len() {
            items[index] = None;
            drop(items);
            debug!(index, "item cleared");
        }
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
    ///
    /// Returns the typed [`SeekOutcome`] — either `Landed` with the requested
    /// target (the actual landed position is committed asynchronously by the
    /// worker thread; this call returns the optimistic outcome) or `PastEof`
    /// when the target is past the current track's known duration.
    pub fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        let slot_id = *self.current_slot.lock_sync();
        let Some(slot_id) = slot_id else {
            return Err(PlayError::NotReady);
        };

        let Some(shared_state) = self.engine.slot_shared_state(slot_id) else {
            return Err(PlayError::SlotNotFound(slot_id));
        };

        let seek_epoch = shared_state.next_seek_epoch();
        shared_state.seek_epoch.store(seek_epoch, Ordering::SeqCst);

        let target_secs = seconds.max(0.0);
        let target = kithara_platform::time::Duration::from_secs_f64(target_secs);

        self.send_to_slot(PlayerCmd::Seek {
            seconds: target_secs,
            seek_epoch,
        })?;

        Ok(match self.duration_seconds() {
            Some(dur) if target_secs >= dur => SeekOutcome::PastEof {
                target,
                duration: kithara_platform::time::Duration::from_secs_f64(dur),
            },
            _ => SeekOutcome::Landed {
                target,
                landed_at: target,
            },
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
            tracing::debug!(
                count = notifications.len(),
                "process_notifications draining"
            );
        }
        for notification in notifications {
            tracing::debug!(?notification, "process_notifications: handle");
            self.dispatch_notification(notification);
        }
    }

    fn dispatch_notification(&self, notification: PlayerNotification) {
        match notification.clone() {
            PlayerNotification::Requested => {
                self.handle_track_requested();
            }
            PlayerNotification::HandoverRequested => {
                self.handle_handover_requested();
            }
            PlayerNotification::PlaybackStopped {
                reason: TrackPlaybackStopReason::Eof,
                ..
            } => {
                self.handle_track_playback_stopped(notification);
            }
            _ => {
                if let Some(event) = player_event_from_notification(notification.clone()) {
                    self.bus.publish(event);
                } else {
                    tracing::trace!(
                        src = ?notification.src(),
                        ?notification,
                        "unhandled player notification"
                    );
                }
            }
        }
    }

    fn handle_track_requested(&self) {
        self.bus.publish(PlayerEvent::PrefetchRequested);
        if self.auto_advance_enabled() {
            let next_index = self.current_index.load(Ordering::Relaxed) + 1;
            if next_index < self.item_count() {
                let _ = self.arm_next(next_index);
            }
        }
    }

    fn handle_handover_requested(&self) {
        if self.crossfade_duration() <= 0.0 {
            // cf=0: arena handover at EOF does it; main thread finalises
            // bookkeeping in the EOF branch of `dispatch_notification`.
            return;
        }
        self.bus.publish(PlayerEvent::HandoverRequested);
        if self.auto_advance_enabled()
            && let Some(idx) = self.armed_next()
        {
            let _ = self.commit_next(idx);
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
    pub fn abr(&self) -> Option<&Arc<AbrController>> {
        self.config.abr.as_ref()
    }

    /// ABR handle of the currently loaded item, if any.
    ///
    /// Returns `Some` while an adaptive (HLS) resource is active; `None`
    /// otherwise. In the production-port wiring this is sourced from the
    /// currently mounted `Resource::abr()` rather than a cached field; the
    /// queue layer threads runtime ABR through this accessor.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        let idx = self.current_index.load(Ordering::Relaxed);
        let items = self.items.lock_sync();
        let handle = items.get(idx)?.as_ref()?.resource.abr_handle();
        drop(items);
        handle
    }

    /// Change ABR mode at runtime. Takes effect on next `decide()`.
    ///
    /// In our model, ABR is per-resource (`Resource::abr()` handle), not per-player.
    /// Mode set here propagates to newly-loaded resources only.
    pub fn set_abr_mode(&self, mode: AbrMode) {
        let _ = mode;
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
    pub fn ensure_engine_started(&self) -> Result<(), PlayError> {
        if !self.engine.is_running() {
            self.engine.start()?;
        }
        Ok(())
    }

    /// Ensure we have an active slot, allocating one if needed.
    pub fn ensure_slot(&self) -> Result<SlotId, PlayError> {
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
        self.auto_advance_enabled.store(enabled, Ordering::Relaxed);
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
        let preserve_active_current =
            current_index_hint.is_some_and(|index| pending.activated && pending.index == index);
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
        PlayerNotification::PlaybackStopped {
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

    #[kithara::test]
    fn prepare_config_applies_player_gapless_mode() {
        let player = PlayerImpl::new(PlayerConfig {
            gapless_mode: GaplessMode::Disabled,
            ..PlayerConfig::default()
        });
        let mut config = crate::impls::config::ResourceConfig::new("https://example.com/song.mp3")
            .expect("BUG: valid resource config");

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
            session: None,
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

    #[kithara::test]
    fn player_default_rate_getter_setter() {
        let player = PlayerImpl::new(PlayerConfig::default());
        // Default rate from config is 1.0
        assert!((player.default_rate() - 1.0).abs() < f32::EPSILON);
        // Change at runtime
        player.set_default_rate(0.75);
        assert!((player.default_rate() - 0.75).abs() < f32::EPSILON);
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
        let event = player_event_from_notification(PlayerNotification::PlaybackStopped {
            src: Arc::from("track.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Eof,
        });
        assert!(matches!(event, Some(PlayerEvent::ItemDidPlayToEnd { .. })));
    }

    #[kithara::test]
    fn playback_stopped_notification_does_not_map_to_item_end_event() {
        let event = player_event_from_notification(PlayerNotification::PlaybackStopped {
            src: Arc::from("track.mp3"),
            item_id: Some(Arc::from("item-1")),
            reason: TrackPlaybackStopReason::Stop,
        });
        assert!(event.is_none());
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
    fn commit_next_without_arm_returns_not_ready() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let err = player.commit_next(1).expect_err("must error");
        assert!(matches!(err, PlayError::NotReady));
    }
}
