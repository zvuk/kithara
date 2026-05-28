use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use bon::Builder;
use kithara_abr::{AbrController, AbrSettings};
use kithara_audio::{AudioWorkerHandle, EqBandConfig, SeekOutcome, generate_log_spaced_bands};
use kithara_bufpool::PcmPool;
use kithara_decode::GaplessMode;
use kithara_events::EventBus;
use kithara_platform::{Mutex, tokio::runtime::Handle as RuntimeHandle};
use portable_atomic::AtomicF32;
use ringbuf::traits::Consumer;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{
    engine::{EngineConfig, EngineImpl},
    player_processor::PlayerCmd,
    player_resource::PlayerResource,
    player_track::TrackTransition,
    session::SessionDispatcher,
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
    src: Arc<str>,
    activated: bool,
    index: usize,
}

/// Configuration for the player.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct PlayerConfig {
    /// How resources created for this player trim leading/trailing PCM.
    #[builder(default)]
    pub gapless_mode: GaplessMode,
    /// Shared ABR controller. When `None`, a default one is created.
    pub abr: Option<Arc<AbrController>>,
    /// Root event bus for this player.
    pub bus: Option<EventBus>,
    /// Master cancel token for this player.
    pub cancel: Option<CancellationToken>,
    /// PCM buffer pool for audio-thread scratch buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Pre-built audio session dispatcher.
    pub session: Option<Arc<dyn SessionDispatcher>>,
    /// EQ band layout. Default: 10-band log-spaced.
    #[builder(default = generate_log_spaced_bands(10))]
    pub eq_layout: Vec<EqBandConfig>,
    /// Built-in auto-advance handler. Default: `true`.
    #[builder(default = true)]
    pub auto_advance_enabled: bool,
    /// Crossfade duration in seconds. Default: 1.0.
    #[builder(default = 1.0)]
    pub crossfade_duration: f32,
    /// Default playback rate (1.0 = normal). Default: 1.0.
    #[builder(default = 1.0)]
    pub default_rate: f32,
    /// Secondary lead time before EOF at which the next queued item is loaded.
    #[builder(default = 3.5)]
    pub prefetch_duration: f32,
    /// Sample rate passed to the engine/runtime backend as a hint.
    /// Default: 44100. Offline/test harnesses set this to drive
    /// deterministic render at a known rate.
    #[builder(default = 44_100)]
    pub sample_rate: u32,
    /// Maximum concurrent slots in the engine. Default: 4.
    #[builder(default = 4)]
    pub max_slots: usize,
}

impl fmt::Debug for PlayerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerConfig")
            .field("gapless_mode", &self.gapless_mode)
            .field("eq_layout", &self.eq_layout)
            .field("auto_advance_enabled", &self.auto_advance_enabled)
            .field("crossfade_duration", &self.crossfade_duration)
            .field("default_rate", &self.default_rate)
            .field("prefetch_duration", &self.prefetch_duration)
            .field("max_slots", &self.max_slots)
            .field("pcm_pool", &self.pcm_pool)
            .finish_non_exhaustive()
    }
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Concrete Player implementation managing items queue.
///
/// Owns an [`EngineImpl`] and sends commands to the active slot's processor.
/// When `play()` is called, the engine is lazily started and a slot is
/// allocated. The current queue item is taken out of the queue, wrapped in
/// [`PlayerResource`], and sent to the processor via `PlayerCmd::LoadTrack`.
pub struct PlayerImpl {
    /// Shared playback rate propagated to the audio pipeline resampler.
    playback_rate_shared: Arc<AtomicF32>,

    auto_advance_enabled: AtomicBool,
    muted: AtomicBool,
    crossfade_duration: AtomicF32,
    default_rate: AtomicF32,
    prefetch_duration: AtomicF32,
    rate: AtomicF32,
    volume: AtomicF32,
    current_index: AtomicUsize,
    /// Master cancel token. Drop fires `cancel.cancel()` so subsystems
    /// observe the pulse before structural Arc teardown unwinds. See
    /// `crates/kithara-play/README.md` "Cancel hierarchy" section.
    cancel: CancellationToken,
    /// Engine drops last — worker shutdown happens after all tracks unregister.
    engine: EngineImpl,
    bus: EventBus,
    /// Live `AbrHandle` of the resource currently in the processor.
    /// Populated by [`Self::enqueue_to_processor`] just before moving
    /// the resource out of `items`, and replaced on every subsequent
    /// load. Lets callers query the active variant after playback has
    /// started — `items[idx]` is `None` then, so the queue-state lookup
    /// can't answer.
    current_abr_handle: Mutex<Option<kithara_abr::AbrHandle>>,
    current_slot: Mutex<Option<SlotId>>,
    /// Items drop before engine — Audio tracks unregister from worker
    /// while it is still alive.
    items: Mutex<Vec<Option<QueuedResource>>>,
    pending_next: Mutex<Option<PendingNext>>,
    status: Mutex<PlayerStatus>,
    pcm_pool: PcmPool,

    config: PlayerConfig,
}

impl PlayerImpl {
    /// Minimum playback rate to prevent stalling.
    const MIN_PLAYBACK_RATE: f32 = 0.01;

    /// Create a new player with the given configuration.
    #[must_use]
    pub fn new(mut config: PlayerConfig) -> Self {
        let resolved_pool = config.pcm_pool.clone().unwrap_or_default();

        let bus = config.bus.clone().unwrap_or_default();

        let cancel = config.cancel.clone().unwrap_or_default(); // kithara:cancel:owner
        config.cancel = Some(cancel.clone());

        let engine_config = EngineConfig {
            eq_layout: config.eq_layout.clone(),
            max_slots: config.max_slots,
            sample_rate: config.sample_rate,
            pcm_pool: Some(resolved_pool.clone()),
            session: config.session.clone(),
            cancel: Some(cancel.clone()),
            ..EngineConfig::default()
        };
        let engine = EngineImpl::new(engine_config, bus.clone());
        if config.abr.is_none() {
            config.abr = Some(AbrController::new(AbrSettings::default()));
        }

        Self::new_with_engine(config, resolved_pool, bus, engine)
    }

    /// Get the shared ABR controller (if configured).
    #[must_use]
    pub fn abr(&self) -> Option<&Arc<AbrController>> {
        self.config.abr.as_ref()
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

    /// Whether the built-in linear auto-advance handler is enabled.
    #[must_use]
    pub fn auto_advance_enabled(&self) -> bool {
        self.auto_advance_enabled.load(Ordering::Relaxed)
    }

    /// Root event bus for this player.
    ///
    /// Use `bus().scoped()` to create a child scope for a resource.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
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

    /// Get player configuration.
    pub fn config(&self) -> &PlayerConfig {
        &self.config
    }

    /// Get crossfade duration in seconds.
    pub fn crossfade_duration(&self) -> f32 {
        self.crossfade_duration.load(Ordering::Relaxed)
    }

    /// ABR handle of the currently loaded item, if any.
    ///
    /// ABR handle of the resource currently running in the processor.
    /// Reads the stash populated by [`Self::enqueue_to_processor`] —
    /// stays valid for the whole life of the track, including after
    /// `items[idx]` has been emptied by the load handoff.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.current_abr_handle.lock_sync().clone()
    }

    /// Current item index in the queue.
    pub fn current_index(&self) -> usize {
        self.current_index.load(Ordering::Relaxed)
    }

    /// Default playback rate used by `play()` and `select_item()`.
    pub fn default_rate(&self) -> f32 {
        self.default_rate.load(Ordering::Relaxed)
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
        state.drain_trash();
        out
    }

    /// Current media duration in seconds.
    ///
    /// Returns `None` while duration is unknown — the engine sets the
    /// shared atomic from the demuxer once mvhd / fmt-equivalent metadata
    /// is parsed. The atomic's default `0.0` conflates "unknown" with
    /// "empty track"; callers that distinguish (e.g. `seek_seconds`'s
    /// `target >= dur` check, queue auto-advance) need the `None` to
    /// avoid false-EOF on a freshly-loaded track whose demuxer has not
    /// yet seen the metadata box.
    pub fn duration_seconds(&self) -> Option<f64> {
        let slot_id = (*self.current_slot.lock_sync())?;
        let state = self.engine.slot_shared_state(slot_id)?;
        let dur = state.duration.load(Ordering::Relaxed);
        (dur > 0.0).then_some(dur)
    }

    /// Get a reference to the underlying engine.
    pub fn engine(&self) -> &EngineImpl {
        &self.engine
    }

    fn enqueue_to_processor(&self, index: usize) -> Option<Arc<str>> {
        let mut items = self.items.lock_sync();
        if index >= items.len() {
            return None;
        }

        let queued = items[index].take()?;
        let QueuedResource { item_id, resource } = queued;

        *self.current_abr_handle.lock_sync() = resource.abr_handle();

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
            item_id,
            resource: arc_resource,
            src: Arc::clone(&src),
        });

        Some(src)
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

    /// Number of EQ bands available for this player.
    pub fn eq_band_count(&self) -> usize {
        self.config.eq_layout.len()
    }

    /// Get EQ gain for a band in dB.
    pub fn eq_gain(&self, band: usize) -> Option<f32> {
        let slot_id = (*self.current_slot.lock_sync())?;
        self.engine.slot_eq(slot_id).and_then(|eq| eq.gain(band))
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

    fn handle_handover_requested(&self) {
        if self.crossfade_duration() <= 0.0 {
            return;
        }
        self.bus.publish(PlayerEvent::HandoverRequested);
        if self.auto_advance_enabled()
            && let Some(idx) = self.armed_next()
        {
            let _ = self.commit_next(idx);
        }
    }

    fn handle_track_playback_stopped(&self, notification: PlayerNotification) {
        if let Some(event) = player_event_from_notification(notification) {
            self.bus.publish(event);
        }

        self.finalize_handover_if_armed();
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

    /// Returns `true` if the player is muted.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
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

    /// Get the number of items in the queue (including consumed items).
    pub fn item_count(&self) -> usize {
        self.items.lock_sync().len()
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

    fn new_with_engine(
        config: PlayerConfig,
        resolved_pool: PcmPool,
        bus: EventBus,
        engine: EngineImpl,
    ) -> Self {
        let cancel = config
            .cancel
            .clone()
            .expect("BUG: PlayerImpl::new / with_engine must populate config.cancel");
        Self {
            crossfade_duration: AtomicF32::new(config.crossfade_duration),
            prefetch_duration: AtomicF32::new(config.prefetch_duration.max(0.0)),
            current_index: AtomicUsize::new(0),
            current_slot: Mutex::new(None),
            default_rate: AtomicF32::new(config.default_rate),
            bus,
            items: Mutex::new(Vec::new()),
            current_abr_handle: Mutex::new(None),
            muted: AtomicBool::new(false),
            pcm_pool: resolved_pool,
            playback_rate_shared: Arc::new(AtomicF32::new(config.default_rate)),
            pending_next: Mutex::new(None),
            auto_advance_enabled: AtomicBool::new(config.auto_advance_enabled),
            rate: AtomicF32::new(0.0),
            status: Mutex::new(PlayerStatus::Unknown),
            volume: AtomicF32::new(1.0),
            cancel,
            engine,
            config,
        }
    }

    /// Pause playback (sets rate to 0.0).
    pub fn pause(&self) {
        self.rate.store(0.0, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
        self.bus.publish(PlayerEvent::RateChanged { rate: 0.0 });
        debug!("pause");
    }

    /// Start playback at the configured default rate.
    pub fn play(&self) {
        let rate = self.default_rate().max(Self::MIN_PLAYBACK_RATE);
        self.rate.store(rate, Ordering::Relaxed);
        self.playback_rate_shared.store(rate, Ordering::Relaxed);

        if let Err(e) = self.ensure_engine_started() {
            warn!(?e, "failed to start engine");
            return;
        }
        if let Err(e) = self.ensure_slot() {
            warn!(?e, "failed to allocate slot");
            return;
        }

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

    /// Insert a resource at the end and immediately crossfade to it.
    pub fn play_resource(&self, resource: Resource) -> Result<(), PlayError> {
        self.insert(resource, None, None);
        let index = self.item_count().saturating_sub(1);
        self.select_item(index, true)
    }

    /// Current playback position in seconds.
    pub fn position_seconds(&self) -> Option<f64> {
        let slot_id = (*self.current_slot.lock_sync())?;
        let state = self.engine.slot_shared_state(slot_id)?;
        Some(state.position.load(Ordering::Relaxed))
    }

    /// Get prefetch lead time in seconds.
    pub fn prefetch_duration(&self) -> f32 {
        self.prefetch_duration.load(Ordering::Relaxed)
    }

    /// Apply shared worker, host sample rate, ABR, and bus to a resource
    /// config so the resource integrates with this player's engine.
    ///
    /// Call this before [`Resource::new`] to ensure the resource shares
    /// the player's decode thread and resampler is pre-initialised with
    /// the correct ratio. Callers that want a shared HTTP pool /
    /// tokio runtime must build their own [`Downloader`] (with an
    /// explicit runtime handle if needed) and attach it via
    /// [`ResourceConfig::for_src`] before passing the config in.
    #[must_use]
    pub fn prepare_config(
        &self,
        config: super::config::ResourceConfig,
    ) -> super::config::ResourceConfig {
        let bus = config.bus.or_else(|| Some(self.bus.scoped()));
        let cancel = config.cancel.or_else(|| Some(self.cancel.child_token()));
        super::config::ResourceConfig {
            bus,
            cancel,
            worker: Some(self.engine.worker().clone()),
            host_sample_rate: std::num::NonZeroU32::new(self.engine.master_sample_rate()),
            gapless_mode: self.config.gapless_mode,
            ..config
        }
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
        state.drain_trash();

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

    /// Current playback rate (0.0 = paused).
    pub fn rate(&self) -> f32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Remove all items from the queue.
    pub fn remove_all_items(&self) {
        self.unarm_next();
        self.items.lock_sync().clear();
        self.current_index.store(0, Ordering::Relaxed);
        self.set_status(PlayerStatus::Unknown);
        debug!("all items removed");
    }

    /// Remove item at index. Returns the removed resource, or `None` if out of bounds
    /// or already consumed.
    pub fn remove_at(&self, index: usize) -> Option<Resource> {
        self.unarm_next();

        let mut items = self.items.lock_sync();
        if index >= items.len() {
            return None;
        }
        let removed = items.remove(index);
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
            seek_epoch,
            seconds: target_secs,
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

    /// Send a command to the current slot's processor.
    fn send_to_slot(&self, cmd: PlayerCmd) -> Result<(), PlayError> {
        let slot_id = (*self.current_slot.lock_sync())
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        self.engine.send_slot_cmd(slot_id, cmd)
    }

    /// Get ducking mode for this player's engine session.
    pub fn session_ducking(&self) -> SessionDuckingMode {
        EngineImpl::session_ducking()
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

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        let clamped = seconds.max(0.0);
        self.crossfade_duration.store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(clamped));
    }

    /// Set the default playback rate used by `play()` and `select_item()`.
    pub fn set_default_rate(&self, rate: f32) {
        self.default_rate.store(rate, Ordering::Relaxed);
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

    /// Set muted state.
    ///
    /// Applies to this player's slot only (per-instance mute).
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
        let effective = if muted { 0.0 } else { self.volume() };
        self.apply_effective_volume(effective);
        self.bus.publish(PlayerEvent::MuteChanged { muted });
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

    /// Set playback rate.
    ///
    /// Updates the local rate and propagates to the audio pipeline resampler
    /// via `PlayerCmd::SetPlaybackRate`. Values below 0.01 are clamped to 0.01.
    pub fn set_rate(&self, rate: f32) {
        let clamped = rate.max(Self::MIN_PLAYBACK_RATE);
        self.rate.store(clamped, Ordering::Relaxed);
        self.playback_rate_shared.store(clamped, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPlaybackRate(clamped));
        self.bus.publish(PlayerEvent::RateChanged { rate: clamped });
    }

    /// Set ducking mode for this player's engine session.
    pub fn set_session_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        EngineImpl::set_session_ducking(mode)
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

    fn start_playback(&self, src: Arc<str>) {
        let _ = self.send_to_slot(PlayerCmd::Transition(TrackTransition::FadeIn(src)));
    }

    /// Get current player status.
    pub fn status(&self) -> PlayerStatus {
        *self.status.lock_sync()
    }

    /// Subscribe to player events.
    ///
    /// Returns an [`EventReceiver`](kithara_events::EventReceiver) that delivers
    /// all events published to this player's root bus (player events, engine
    /// events, and resource events from all items).
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }

    /// Pump audio backend/runtime state.
    pub fn tick(&self) -> Result<(), PlayError> {
        self.engine.tick()
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

    /// Get current volume (0.0..=1.0).
    pub fn volume(&self) -> f32 {
        self.volume.load(Ordering::Relaxed)
    }

    /// Shared audio worker handle for this player's engine.
    ///
    /// Clone and pass to [`ResourceConfig::with_worker`] so resources
    /// loaded into this player share a single decode thread.
    #[must_use]
    pub fn worker(&self) -> &AudioWorkerHandle {
        self.engine.worker()
    }
}

fn player_event_from_notification(notification: PlayerNotification) -> Option<PlayerEvent> {
    match notification {
        PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Eof,
            src,
            item_id,
        } => Some(PlayerEvent::ItemDidPlayToEnd { src, item_id }),
        PlayerNotification::PlaybackStopped {
            reason: TrackPlaybackStopReason::Failed,
            src,
            item_id,
        } => Some(PlayerEvent::ItemDidFail { src, item_id }),
        _ => None,
    }
}

impl Drop for PlayerImpl {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl crate::traits::dj::eq::Equalizer for PlayerImpl {
    fn band_count(&self) -> usize {
        self.eq_band_count()
    }

    fn gain(&self, band: usize) -> Option<f32> {
        self.eq_gain(band)
    }

    fn reset(&self) -> Result<(), PlayError> {
        self.reset_eq()
    }

    fn set_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError> {
        self.set_eq_gain(band, gain_db)
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

        config = player.prepare_config(config);

        assert_eq!(config.gapless_mode, GaplessMode::Disabled);
        assert!(
            config.cancel.is_some(),
            "prepare_config must inject a per-track cancel child"
        );
        player.worker().shutdown();
    }

    #[kithara::test]
    fn prepare_config_per_track_cancel_is_child_of_player_master() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let mut rc = crate::impls::config::ResourceConfig::new("https://example.com/song.mp3")
            .expect("BUG: valid resource config");
        rc = player.prepare_config(rc);

        let track_cancel = rc.cancel.expect("prepare_config must populate cancel");
        let observer = track_cancel.child_token();
        assert!(!observer.is_cancelled());

        drop(player);
        assert!(
            observer.is_cancelled(),
            "dropping the player must cancel the per-track child via the master"
        );
    }

    #[kithara::test]
    fn prepare_config_preserves_caller_supplied_master() {
        let parent_master = CancellationToken::new();
        let player = PlayerImpl::new(PlayerConfig {
            cancel: Some(parent_master.clone()),
            ..PlayerConfig::default()
        });
        let mut rc = crate::impls::config::ResourceConfig::new("https://example.com/song.mp3")
            .expect("BUG: valid resource config");
        rc = player.prepare_config(rc);

        let track_cancel = rc.cancel.expect("prepare_config must populate cancel");
        let observer = track_cancel.child_token();
        assert!(!observer.is_cancelled());

        parent_master.cancel();
        assert!(observer.is_cancelled());
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
        assert!((player.prefetch_duration() - 3.5).abs() < f32::EPSILON);
        player.set_prefetch_duration(8.0);
        assert!((player.prefetch_duration() - 8.0).abs() < f32::EPSILON);
        player.set_prefetch_duration(-1.0);
        assert!((player.prefetch_duration() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_events_subscribe() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let mut rx = player.subscribe();
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
            sample_rate: 44_100,
            pcm_pool: None,
            auto_advance_enabled: true,
            session: None,
            cancel: None,
        };
        let player = PlayerImpl::new(config);
        assert!((player.crossfade_duration() - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn player_config_builder() {
        let config = PlayerConfig::builder()
            .max_slots(8)
            .default_rate(0.5)
            .crossfade_duration(2.5)
            .prefetch_duration(7.0)
            .eq_layout(generate_log_spaced_bands(5))
            .build();
        assert_eq!(config.max_slots, 8);
        assert!((config.default_rate - 0.5).abs() < f32::EPSILON);
        assert!((config.crossfade_duration - 2.5).abs() < f32::EPSILON);
        assert!((config.prefetch_duration - 7.0).abs() < f32::EPSILON);
        assert_eq!(config.eq_layout.len(), 5);
    }

    #[kithara::test]
    fn player_default_rate_getter_setter() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!((player.default_rate() - 1.0).abs() < f32::EPSILON);
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
        let shared = &player.playback_rate_shared;
        assert!((shared.load(Ordering::Relaxed) - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn set_rate_clamps_invalid_values() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_rate(0.0);
        assert!(player.rate() >= 0.01);
        assert!(player.playback_rate_shared.load(Ordering::Relaxed) >= 0.01);

        player.set_rate(-1.0);
        assert!(player.rate() >= 0.01);
    }

    #[kithara::test]
    fn player_exposes_worker() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let _w = player.worker();
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
