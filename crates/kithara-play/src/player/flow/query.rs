use std::{num::NonZeroU32, sync::Arc};

use kithara_audio::{AudioWorkerHandle, EngineLoadSnapshot};
use kithara_events::EventBus;
use kithara_platform::tokio::runtime::Handle as RuntimeHandle;

use super::super::core::PlayerImpl;
use crate::{
    api::{PlayerStatus, SessionDuckingMode, SlotId},
    bridge::PlaybackSnapshot,
    engine::EngineImpl,
    error::PlayError,
    resource::ResourceConfig,
};

impl PlayerImpl {
    /// Whether the built-in linear auto-advance handler is enabled.
    #[must_use]
    pub fn auto_advance_enabled(&self) -> bool {
        self.core.params.auto_advance_enabled()
    }

    /// Root event bus for this player.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        self.core.engine.bus()
    }

    /// Get crossfade duration in seconds.
    pub fn crossfade_duration(&self) -> f32 {
        self.core.params.crossfade_duration()
    }

    /// Current item index in the queue.
    pub fn current_index(&self) -> usize {
        self.core.playlist.lock().current()
    }

    /// Default playback rate used by `play()` and `select_item()`.
    pub fn default_rate(&self) -> f32 {
        self.core.params.default_rate()
    }

    /// ABR handle of the currently loaded item, if any.
    ///
    /// Reads the stash populated by `enqueue_to_processor` — stays valid for
    /// the whole life of the track, including after `items[idx]` has been
    /// emptied by the load handoff.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.phase.lock().abr_handle()
    }

    /// Get a reference to the underlying engine.
    pub fn engine(&self) -> &EngineImpl {
        &self.core.engine
    }

    /// Live cost snapshot of the audio engine (decode + effects).
    #[must_use]
    pub fn engine_load(&self) -> EngineLoadSnapshot {
        self.core.engine_load.snapshot()
    }

    /// Notify the audio host that the platform route changed and the
    /// native output stream must be recreated if playback is active.
    pub fn invalidate_audio_route(&self, reason: &str) -> Result<(), PlayError> {
        self.core.engine.invalidate_audio_route(reason)
    }

    /// Number of EQ bands available for this player.
    pub fn eq_band_count(&self) -> usize {
        self.core.engine.eq_band_count()
    }

    /// Current media duration in seconds.
    ///
    /// Returns `None` while duration is unknown — the engine sets the shared
    /// atomic from the demuxer once mvhd / fmt-equivalent metadata is parsed.
    /// The atomic's default `0.0` conflates "unknown" with "empty track";
    /// callers that distinguish (e.g. `seek_seconds`'s `target >= dur` check,
    /// queue auto-advance) need the `None` to avoid false-EOF on a freshly-
    /// loaded track whose demuxer has not yet seen the metadata box.
    pub fn duration_seconds(&self) -> Option<f64> {
        let dur = self.playback_snapshot()?.duration;
        (dur > 0.0).then_some(dur)
    }

    /// Ensure we have an active slot, allocating one if needed.
    pub fn ensure_slot(&self) -> Result<SlotId, PlayError> {
        if let Some(id) = self.slot() {
            return Ok(id);
        }
        let id = self.core.engine.allocate_slot()?;
        self.enter_loading_with_slot(id);
        let effective = if self.is_muted() { 0.0 } else { self.volume() };
        self.core.engine.set_slot_volume(id, effective)?;
        Ok(id)
    }

    /// Get EQ gain for a band in dB.
    pub fn eq_gain(&self, band: usize) -> Option<f32> {
        let slot_id = self.slot()?;
        self.core
            .engine
            .slot_eq(slot_id)
            .and_then(|eq| eq.gain(band))
    }

    /// Returns `true` if the player is in playing state.
    pub fn is_playing(&self) -> bool {
        self.playback_snapshot().is_some_and(|s| s.playing)
    }

    /// Returns `true` if the player is muted.
    pub fn is_muted(&self) -> bool {
        self.core.params.is_muted()
    }

    /// Get the number of items in the queue (including consumed items).
    pub fn item_count(&self) -> usize {
        self.core.playlist.lock().len()
    }

    /// Single coherent read of the active slot's live playback scalars.
    ///
    /// `None` when no slot is allocated. The standalone `position_seconds`
    /// / `duration_seconds` / `is_playing` / `buffered_seconds` getters are
    /// thin derivations of this snapshot — one shared read primitive.
    pub fn playback_snapshot(&self) -> Option<PlaybackSnapshot> {
        let slot_id = self.slot()?;
        Some(self.core.engine.slot_playback(slot_id)?.snapshot())
    }

    /// Current playback position in seconds.
    pub fn position_seconds(&self) -> Option<f64> {
        Some(self.playback_snapshot()?.position)
    }

    /// Get prefetch lead time in seconds.
    pub fn prefetch_duration(&self) -> f32 {
        self.core.params.prefetch_duration()
    }

    /// Apply shared worker, host sample rate, ABR, and bus to a resource
    /// config so the resource integrates with this player's engine.
    ///
    /// Call this before [`Resource::new`](crate::resource::Resource::new) to
    /// ensure the resource shares the player's decode thread and resampler is
    /// pre-initialised with the correct ratio. Callers that want a shared HTTP
    /// pool / tokio runtime must build their own downloader and attach it via
    /// [`ResourceConfig::with_downloader`] before passing the config in.
    #[must_use]
    pub fn prepare_config(&self, config: ResourceConfig) -> ResourceConfig {
        let bus = config.bus.or_else(|| Some(self.core.engine.bus().scoped()));
        let cancel = config
            .cancel
            .or_else(|| self.core.engine.cancel_token())
            .map(|parent| parent.child());
        let stretch = Some(Arc::clone(&self.core.timestretch));
        let host_sample_rate = NonZeroU32::new(self.core.engine.master_sample_rate())
            .or_else(|| NonZeroU32::new(self.core.engine.configured_sample_rate()));
        ResourceConfig {
            bus,
            cancel,
            worker: Some(self.core.engine.worker().clone()),
            host_sample_rate,
            gapless_mode: self.core.gapless_mode,
            stretch,
            engine_load: Some(Arc::clone(&self.core.engine_load)),
            ..config
        }
    }

    /// Current playback rate (0.0 = paused).
    pub fn rate(&self) -> f32 {
        self.core.params.rate()
    }

    /// Runtime handle captured by this player's engine.
    #[must_use]
    pub fn runtime(&self) -> Option<&RuntimeHandle> {
        self.core.engine.runtime()
    }

    /// Get ducking mode for this player's engine session.
    pub fn session_ducking(&self) -> SessionDuckingMode {
        EngineImpl::session_ducking()
    }

    /// Get current player status.
    pub fn status(&self) -> PlayerStatus {
        *self.core.status.lock()
    }

    /// Subscribe to player events.
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.core.engine.bus().subscribe()
    }

    /// Pump audio backend/runtime state.
    pub fn tick(&self) -> Result<(), PlayError> {
        self.core.engine.tick()
    }

    /// Get current volume (0.0..=1.0).
    pub fn volume(&self) -> f32 {
        self.core.params.volume()
    }

    /// Shared audio worker handle for this player's engine.
    #[must_use]
    pub fn worker(&self) -> &AudioWorkerHandle {
        self.core.engine.worker()
    }
}
