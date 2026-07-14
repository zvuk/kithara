use std::{
    fmt,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use bon::Builder;
use kithara_abr::{AbrController, AbrSettings};
use kithara_audio::{
    AudioWorkerHandle, EngineLoad, EngineLoadSnapshot, EqBandConfig, StretchControls,
    generate_log_spaced_bands,
};
use kithara_bufpool::PcmPool;
use kithara_decode::GaplessMode;
use kithara_events::EventBus;
use kithara_platform::{
    CancelScope, CancelToken,
    sync::{Arc, Mutex},
    tokio::runtime::Handle as RuntimeHandle,
};
use portable_atomic::AtomicF32;
use tracing::debug;

use super::phase::{PlayerPhase, QueuedResource};
use crate::{
    error::PlayError,
    events::PlayerEvent,
    impls::{
        engine::{EngineConfig, EngineImpl},
        resource::Resource,
        session::SessionDispatcher,
    },
    traits::engine::Engine,
    types::{PlayerStatus, SessionDuckingMode},
};

/// Configuration for the player.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct PlayerConfig {
    /// Per-deck time-stretch control handle, shared with the UI and the
    /// worker effect chain (see `kithara_audio::StretchControls`).
    #[builder(default = StretchControls::new(1.0))]
    pub timestretch: Arc<StretchControls>,
    /// How resources created for this player trim leading/trailing PCM.
    #[builder(default)]
    pub gapless_mode: GaplessMode,
    /// Shared ABR controller. When `None`, a default one is created.
    pub abr: Option<Arc<AbrController>>,
    /// Root event bus for this player.
    pub bus: Option<EventBus>,
    /// Master cancel token for this player.
    pub cancel: Option<CancelToken>,
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

/// Phase-neutral state shared across every player phase.
///
/// Field order is drop order: `items` (holding undelivered resources that
/// carry worker references) drops before `engine`, and `engine` (whose
/// `Drop` shuts the worker down) drops last.
pub(crate) struct PlayerCore {
    /// Live shared cost meter of the audio engine (decode + effects).
    /// Constructed once and kept address-stable for the player's lifetime.
    pub(crate) engine_load: Arc<EngineLoad>,

    pub(crate) auto_advance_enabled: AtomicBool,
    pub(crate) muted: AtomicBool,
    pub(crate) crossfade_duration: AtomicF32,
    pub(crate) default_rate: AtomicF32,
    pub(crate) prefetch_duration: AtomicF32,
    pub(crate) rate: AtomicF32,
    pub(crate) volume: AtomicF32,
    pub(crate) current_index: AtomicUsize,
    /// Index last announced via `CurrentItemChanged` (sentinel `usize::MAX`
    /// until the first announce).
    pub(crate) last_announced_index: AtomicUsize,
    /// Master cancel token. Drop fires `cancel.cancel()` so subsystems
    /// observe the pulse before structural Arc teardown unwinds. See
    /// `crates/kithara-play/CONTEXT.md` "Cancel hierarchy" section.
    pub(crate) cancel: CancelToken,
    /// Engine drops last — worker shutdown happens after all tracks
    /// unregister and after `items` releases their resources.
    pub(crate) engine: EngineImpl,
    pub(crate) bus: EventBus,
    /// Items drop before engine — Audio tracks unregister from worker
    /// while it is still alive.
    pub(crate) items: Mutex<Vec<Option<QueuedResource>>>,
    /// Status kept explicit (not derived from phase): `set_status` emits
    /// `StatusChanged` only on change and its values are not 1:1 with phase.
    pub(crate) status: Mutex<PlayerStatus>,
    pub(crate) pcm_pool: PcmPool,
    pub(crate) config: PlayerConfig,
}

/// Concrete Player implementation managing items queue.
///
/// Owns an [`EngineImpl`] and sends commands to the active slot's processor.
/// When `play()` is called, the engine is lazily started and a slot is
/// allocated. The current queue item is taken out of the queue, wrapped in
/// [`PlayerResource`](crate::impls::player_resource::PlayerResource), and sent
/// to the processor via `PlayerCmd::LoadTrack`.
///
/// Internally the player is a phase-split typestate: `phase` is a typed
/// `Mutex<PlayerPhase>` carrying the slot / ABR handle / armed-next, while
/// `core` holds the phase-neutral fields. `phase` is declared first so it
/// drops before `core.engine`.
pub struct PlayerImpl {
    pub(crate) phase: Mutex<PlayerPhase>,
    pub(crate) core: PlayerCore,
}

impl PlayerImpl {
    /// Minimum playback rate to prevent stalling.
    pub(crate) const MIN_PLAYBACK_RATE: f32 = 0.01;

    /// Create a new player with the given configuration.
    #[must_use]
    pub fn new(mut config: PlayerConfig) -> Self {
        let resolved_pool = config.pcm_pool.clone().unwrap_or_default();

        let bus = config.bus.clone().unwrap_or_default();

        // Composed/standalone seam: `Some(parent)` → the player's master is a
        // child of it (so a passed cancel reaches the player but the player's
        // Drop never cancels the passed token); `None` → own root.
        let cancel = CancelScope::new(config.cancel.clone()).token();
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
            config.abr = Some(AbrController::new(AbrSettings::default(), cancel.child()));
        }

        Self::new_with_engine(config, resolved_pool, bus, engine)
    }

    /// Advance to the next item in the queue.
    ///
    /// Does nothing if the current item is already the last one.
    pub fn advance_to_next_item(&self) {
        let items = self.core.items.lock();
        let current = self.core.current_index.load(Ordering::Relaxed);
        if current + 1 < items.len() {
            self.core
                .current_index
                .store(current + 1, Ordering::Relaxed);
            drop(items);
            self.announce_current_item(current + 1);
            debug!(new_index = current + 1, "advanced to next item");
        }
    }

    /// Sole publisher of `CurrentItemChanged`: emits only when `index` differs
    /// from the last announced item, so a `play()` resume of the same item
    /// stays quiet.
    pub(crate) fn announce_current_item(&self, index: usize) {
        if self
            .core
            .last_announced_index
            .swap(index, Ordering::Relaxed)
            != index
        {
            self.core.bus.publish(PlayerEvent::CurrentItemChanged);
        }
    }

    /// Whether the built-in linear auto-advance handler is enabled.
    #[must_use]
    pub fn auto_advance_enabled(&self) -> bool {
        self.core.auto_advance_enabled.load(Ordering::Relaxed)
    }

    delegate::delegate! {
        to self.core {
            /// Root event bus for this player.
            ///
            /// Use `bus().scoped()` to create a child scope for a resource.
            #[must_use]
            #[field(&bus)]
            pub fn bus (& self) -> & EventBus;
            /// Get player configuration.
            #[field(&config)]
            pub fn config (& self) -> & PlayerConfig;
            /// Get a reference to the underlying engine.
            #[field(&engine)]
            pub fn engine (& self) -> & EngineImpl;
        }
        to self.core.engine {
            /// Notify the audio host that the platform route changed and the
            /// native output stream must be recreated if playback is active.
            pub fn invalidate_audio_route (& self , reason : & str) -> Result < () , PlayError >;
            /// Runtime handle captured by this player's engine.
            ///
            /// Use when building a shared
            /// [`Downloader`](kithara_stream::dl::Downloader) so its async tasks
            /// land on the same runtime the audio engine observes, then pass the
            /// downloader through
            /// [`ResourceConfig::with_downloader`](crate::impls::config::ResourceConfig::with_downloader).
            #[must_use]
            pub fn runtime (& self) -> Option < & RuntimeHandle >;
            /// Pump audio backend/runtime state.
            pub fn tick (& self) -> Result < () , PlayError >;
            /// Shared audio worker handle for this player's engine.
            ///
            /// Clone and pass to
            /// [`ResourceConfig::with_worker`](crate::impls::config::ResourceConfig::with_worker)
            /// so resources loaded into this player share a single decode thread.
            #[must_use]
            pub fn worker (& self) -> & AudioWorkerHandle;
        }
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
        let mut items = self.core.items.lock();
        if index < items.len() {
            items[index] = None;
            drop(items);
            debug!(index, "item cleared");
        }
    }

    /// Get crossfade duration in seconds.
    pub fn crossfade_duration(&self) -> f32 {
        self.core.crossfade_duration.load(Ordering::Relaxed)
    }

    /// Current item index in the queue.
    pub fn current_index(&self) -> usize {
        self.core.current_index.load(Ordering::Relaxed)
    }

    /// Default playback rate used by `play()` and `select_item()`.
    pub fn default_rate(&self) -> f32 {
        self.core.default_rate.load(Ordering::Relaxed)
    }

    /// Live cost snapshot of the audio engine (decode + effects).
    #[must_use]
    pub fn engine_load(&self) -> EngineLoadSnapshot {
        self.core.engine_load.snapshot()
    }

    /// Ensure the audio engine is started.
    ///
    /// Idempotent under concurrency: if another thread wins the start race
    /// (the engine is now running), `Engine::start` returns
    /// `EngineAlreadyRunning`, which is the success case here — the engine
    /// is started, which is all this method promises.
    pub fn ensure_engine_started(&self) -> Result<(), PlayError> {
        if self.core.engine.is_running() {
            return Ok(());
        }
        match self.core.engine.start() {
            Ok(()) | Err(PlayError::EngineAlreadyRunning) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Number of EQ bands available for this player.
    pub fn eq_band_count(&self) -> usize {
        self.core.config.eq_layout.len()
    }

    /// Insert a resource with optional queue-item identity metadata at a
    /// specific position, or append to the end.
    pub fn insert(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        at_position: Option<usize>,
    ) {
        let mut items = self.core.items.lock();
        let pos = at_position.map_or(items.len(), |i| i.min(items.len()));
        items.insert(pos, Some(QueuedResource { item_id, resource }));
        debug!(count = items.len(), pos, "item inserted");
    }

    /// Returns `true` if the player is muted.
    pub fn is_muted(&self) -> bool {
        self.core.muted.load(Ordering::Relaxed)
    }

    /// Get the number of items in the queue (including consumed items).
    pub fn item_count(&self) -> usize {
        self.core.items.lock().len()
    }

    pub(crate) fn new_with_engine(
        config: PlayerConfig,
        resolved_pool: PcmPool,
        bus: EventBus,
        engine: EngineImpl,
    ) -> Self {
        let cancel = config
            .cancel
            .clone()
            .expect("BUG: PlayerImpl::new / with_engine must populate config.cancel");
        // Seed the single speed source with the configured default rate.
        config.timestretch.set_speed(config.default_rate);
        let core = PlayerCore {
            engine_load: Arc::new(EngineLoad::default()),
            auto_advance_enabled: AtomicBool::new(config.auto_advance_enabled),
            muted: AtomicBool::new(false),
            crossfade_duration: AtomicF32::new(config.crossfade_duration),
            default_rate: AtomicF32::new(config.default_rate),
            prefetch_duration: AtomicF32::new(config.prefetch_duration.max(0.0)),
            rate: AtomicF32::new(0.0),
            volume: AtomicF32::new(1.0),
            current_index: AtomicUsize::new(0),
            last_announced_index: AtomicUsize::new(usize::MAX),
            cancel,
            bus,
            status: Mutex::default(),
            pcm_pool: resolved_pool,
            config,
            items: Mutex::default(),
            engine,
        };
        Self {
            core,
            phase: Mutex::new(PlayerPhase::Idle),
        }
    }

    /// Get prefetch lead time in seconds.
    pub fn prefetch_duration(&self) -> f32 {
        self.core.prefetch_duration.load(Ordering::Relaxed)
    }

    /// Apply shared worker, host sample rate, ABR, and bus to a resource
    /// config so the resource integrates with this player's engine.
    ///
    /// Call this before [`Resource::new`] to ensure the resource shares
    /// the player's decode thread and resampler is pre-initialised with
    /// the correct ratio. Callers that want a shared HTTP pool /
    /// tokio runtime must build their own [`Downloader`](kithara_stream::dl::Downloader)
    /// (with an explicit runtime handle if needed) and attach it via
    /// [`ResourceConfig::for_src`](crate::impls::config::ResourceConfig::for_src)
    /// before passing the config in.
    #[must_use]
    pub fn prepare_config(
        &self,
        config: crate::impls::config::ResourceConfig,
    ) -> crate::impls::config::ResourceConfig {
        let bus = config.bus.or_else(|| Some(self.core.bus.scoped()));
        // Give each prepared resource a fresh per-instance child so one
        // track's teardown never cancels a sibling. Derives from the
        // caller-provided parent (e.g. the app/queue master) or from this
        // player's master when none was supplied.
        let parent = config.cancel.unwrap_or_else(|| self.core.cancel.clone());
        let cancel = Some(parent.child());
        // Always pass the shared speed controls: with a stretch backend
        // compiled in, the stretch slot owns speed and key-lock live. Without
        // one, PCM speed is pinned and the controls remain state only.
        let stretch = Some(Arc::clone(&self.core.config.timestretch));
        let mut decoder = config.decoder.clone();
        decoder.gapless_mode = self.core.config.gapless_mode;
        crate::impls::config::ResourceConfig {
            bus,
            cancel,
            worker: Some(self.core.engine.worker().clone()),
            host_sample_rate: std::num::NonZeroU32::new(self.core.engine.master_sample_rate()),
            decoder,
            stretch,
            engine_load: Some(Arc::clone(&self.core.engine_load)),
            ..config
        }
    }

    /// Current playback rate (0.0 = paused).
    pub fn rate(&self) -> f32 {
        self.core.rate.load(Ordering::Relaxed)
    }

    /// Remove item at index. Returns the removed resource, or `None` if out of
    /// bounds or already consumed.
    pub fn remove_at(&self, index: usize) -> Option<Resource> {
        self.unarm_next();

        let mut items = self.core.items.lock();
        if index >= items.len() {
            return None;
        }
        let removed = items.remove(index);
        let current = self.core.current_index.load(Ordering::Relaxed);
        if index < current {
            self.core
                .current_index
                .store(current.saturating_sub(1), Ordering::Relaxed);
        } else if index == current && current >= items.len() && !items.is_empty() {
            self.core
                .current_index
                .store(items.len() - 1, Ordering::Relaxed);
        }
        // A removal shifts indices, so re-open announce (index match is stale).
        self.core
            .last_announced_index
            .store(usize::MAX, Ordering::Relaxed);
        debug!(index, remaining = items.len(), "item removed");
        removed.map(|queued| queued.resource)
    }

    /// Replace a consumed (or existing) resource at the given index.
    ///
    /// Use this to re-load a track that was previously played and consumed
    /// by `load_current_item`. Does nothing if `index` is out of bounds.
    pub fn replace_item(&self, index: usize, resource: Resource) {
        self.replace_item_tagged(index, resource, None);
    }

    /// Replace a consumed (or existing) resource at the given index with item
    /// identity metadata.
    pub fn replace_item_tagged(&self, index: usize, resource: Resource, item_id: Option<Arc<str>>) {
        let mut items = self.core.items.lock();
        if index < items.len() {
            items[index] = Some(QueuedResource { item_id, resource });
            drop(items);
            // Replacing the announced item re-opens announce for the next play.
            if index == self.core.last_announced_index.load(Ordering::Relaxed) {
                self.core
                    .last_announced_index
                    .store(usize::MAX, Ordering::Relaxed);
            }
            debug!(index, "item replaced");
        }
    }

    /// Pre-allocate empty slots so `replace_item` can fill them by index.
    pub fn reserve_slots(&self, count: usize) {
        self.core.items.lock().resize_with(count, || None);
        debug!(count, "slots reserved");
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
        self.core
            .auto_advance_enabled
            .store(enabled, Ordering::Relaxed);
    }

    /// Set the default playback rate used by `play()` and `select_item()`.
    pub fn set_default_rate(&self, rate: f32) {
        self.core.default_rate.store(rate, Ordering::Relaxed);
    }

    /// Set muted state.
    ///
    /// Applies to this player's slot only (per-instance mute).
    pub fn set_muted(&self, muted: bool) {
        self.core.muted.store(muted, Ordering::Relaxed);
        let effective = if muted { 0.0 } else { self.volume() };
        self.apply_effective_volume(effective);
        self.core.bus.publish(PlayerEvent::MuteChanged { muted });
    }

    /// Set ducking mode for this player's engine session.
    pub fn set_session_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        EngineImpl::set_session_ducking(mode)
    }

    /// Internal: set status and emit event if changed.
    pub(crate) fn set_status(&self, new_status: PlayerStatus) {
        let mut status = self.core.status.lock();
        if *status != new_status {
            *status = new_status;
            drop(status);
            self.core
                .bus
                .publish(PlayerEvent::StatusChanged { status: new_status });
        }
    }

    /// Set volume, clamped to `0.0..=1.0`.
    ///
    /// Applies to this player's slot only (per-instance volume).
    pub fn set_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.core.volume.store(clamped, Ordering::Relaxed);
        if !self.is_muted() {
            self.apply_effective_volume(clamped);
        }
        self.core
            .bus
            .publish(PlayerEvent::VolumeChanged { volume: clamped });
    }

    /// Get current player status.
    pub fn status(&self) -> PlayerStatus {
        *self.core.status.lock()
    }

    /// Subscribe to player events.
    ///
    /// Returns an [`EventReceiver`](kithara_events::EventReceiver) that delivers
    /// all events published to this player's root bus (player events, engine
    /// events, and resource events from all items).
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.core.bus.subscribe()
    }

    /// Get current volume (0.0..=1.0).
    pub fn volume(&self) -> f32 {
        self.core.volume.load(Ordering::Relaxed)
    }
}

impl Drop for PlayerImpl {
    fn drop(&mut self) {
        self.core.cancel.cancel();
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
    use kithara_events::Event;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        impls::{player_processor::PlayerCmd, session::testing},
        types::SlotId,
    };

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

        assert_eq!(config.decoder.gapless_mode, GaplessMode::Disabled);
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
        let observer = track_cancel.child();
        assert!(!observer.is_cancelled());

        drop(player);
        assert!(
            observer.is_cancelled(),
            "dropping the player must cancel the per-track child via the master"
        );
    }

    #[kithara::test]
    fn prepare_config_preserves_caller_supplied_master() {
        let parent_master = CancelToken::never();
        let player = PlayerImpl::new(PlayerConfig {
            cancel: Some(parent_master.clone()),
            ..PlayerConfig::default()
        });
        let mut rc = crate::impls::config::ResourceConfig::new("https://example.com/song.mp3")
            .expect("BUG: valid resource config");
        rc = player.prepare_config(rc);

        let track_cancel = rc.cancel.expect("prepare_config must populate cancel");
        let observer = track_cancel.child();
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
        player.core.rate.store(1.0, Ordering::Relaxed);
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
        let _lock = crate::impls::engine::ducking_test_lock().lock();
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
            timestretch: StretchControls::new(1.0),
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
    fn set_rate_updates_shared_speed() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_rate(2.0);
        assert!((player.core.config.timestretch.speed() - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn set_rate_clamps_invalid_values() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_rate(0.0);
        assert!(player.rate() >= 0.01);
        assert!(player.core.config.timestretch.speed() >= 0.01);

        player.set_rate(-1.0);
        assert!(player.rate() >= 0.01);
    }

    #[kithara::test]
    fn timestretch_is_address_stable_across_play_pause() {
        let player = PlayerImpl::new(PlayerConfig {
            session: Some(testing::test_session()),
            ..PlayerConfig::default()
        });
        let ptr_before = Arc::as_ptr(&player.core.config.timestretch);
        player.play();
        player.pause();
        player.play();
        let ptr_after = Arc::as_ptr(&player.core.config.timestretch);
        assert_eq!(
            ptr_before, ptr_after,
            "timestretch controls must stay address-stable across transitions"
        );
        player.worker().shutdown();
    }

    #[kithara::test]
    fn pause_from_idle_is_noop() {
        use super::super::phase::PlayerPhaseKind;

        let player = PlayerImpl::new(PlayerConfig::default());
        assert_eq!(player.phase_kind(), PlayerPhaseKind::Idle);
        player.pause();
        assert_eq!(
            player.phase_kind(),
            PlayerPhaseKind::Idle,
            "pause from Idle must not leak a phase transition"
        );
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn position_seconds_idle_is_none() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!(player.position_seconds().is_none());
        assert!(player.duration_seconds().is_none());
        assert!(!player.is_playing());
        assert!(player.current_abr_handle().is_none());
        assert!(player.armed_next().is_none());
    }

    #[kithara::test]
    fn drop_player_releases_tracks_before_engine() {
        // Worker-registered tracks live in `engine`; undelivered resources in
        // `items` carry worker references. The phase (slot/abr/pending) holds
        // no worker-registered track directly. This pins that constructing,
        // arming a phase, and dropping does not panic / UAF: `phase` and
        // `core.items` must drop before `core.engine`.
        let player = PlayerImpl::new(PlayerConfig::default());
        *player.phase.lock() = PlayerPhase::Playing {
            slot: SlotId::new(0),
            abr_handle: None,
            pending: None,
        };
        player.worker().shutdown();
        drop(player);
    }

    #[kithara::test]
    fn set_rate_emits_rate_changed() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let mut rx = player.subscribe();
        player.set_rate(2.0);
        let e = rx.try_recv();
        assert!(matches!(
            e,
            Ok(Event::Player(PlayerEvent::RateChanged { .. }))
        ));
    }

    #[kithara::test]
    fn player_exposes_worker() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let _w = player.worker();
        let _w2 = player.worker().clone();
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
}
