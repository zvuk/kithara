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
use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_platform::{Mutex, tokio::sync::broadcast};
use portable_atomic::AtomicF32;
use ringbuf::traits::Consumer;
use tracing::{debug, warn};

use super::{
    engine::{EngineConfig, EngineImpl},
    player_processor::PlayerCmd,
    player_resource::PlayerResource,
    player_track::TrackTransition,
};
use crate::{
    error::PlayError,
    events::PlayerEvent,
    impls::resource::Resource,
    traits::engine::Engine,
    types::{ActionAtItemEnd, PlayerStatus, SessionDuckingMode, SlotId},
};

/// Configuration for the player.
#[derive(Clone, Debug, Derivative, Setters)]
#[derivative(Default)]
#[setters(prefix = "with_", strip_option)]
pub struct PlayerConfig {
    /// Crossfade duration in seconds. Default: 1.0.
    #[derivative(Default(value = "1.0"))]
    pub crossfade_duration: f32,
    /// Default playback rate (1.0 = normal). Default: 1.0.
    #[derivative(Default(value = "1.0"))]
    pub default_rate: f32,
    /// Number of EQ bands. Default: 10.
    #[derivative(Default(value = "10"))]
    pub eq_bands: usize,
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
    engine: EngineImpl,

    action_at_item_end: Mutex<ActionAtItemEnd>,
    crossfade_duration: AtomicF32,
    current_index: AtomicUsize,
    current_slot: Mutex<Option<SlotId>>,
    default_rate: AtomicF32,
    events_tx: broadcast::Sender<PlayerEvent>,
    items: Mutex<Vec<Option<Resource>>>,
    muted: AtomicBool,
    pcm_pool: PcmPool,
    rate: AtomicF32,
    status: Mutex<PlayerStatus>,
    volume: AtomicF32,
}

impl PlayerImpl {
    /// Create a new player with the given configuration.
    #[must_use]
    pub fn new(config: PlayerConfig) -> Self {
        let resolved_pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| pcm_pool().clone());

        let engine_config = EngineConfig {
            eq_bands: config.eq_bands,
            max_slots: config.max_slots,
            pcm_pool: Some(resolved_pool.clone()),
            ..EngineConfig::default()
        };
        let engine = EngineImpl::new(engine_config);

        let (events_tx, _) = broadcast::channel(64);
        Self {
            action_at_item_end: Mutex::new(ActionAtItemEnd::default()),
            crossfade_duration: AtomicF32::new(config.crossfade_duration),
            current_index: AtomicUsize::new(0),
            current_slot: Mutex::new(None),
            default_rate: AtomicF32::new(config.default_rate),
            events_tx,
            items: Mutex::new(Vec::new()),
            muted: AtomicBool::new(false),
            pcm_pool: resolved_pool,
            rate: AtomicF32::new(0.0), // starts paused
            status: Mutex::new(PlayerStatus::Unknown),
            volume: AtomicF32::new(1.0),
            engine,
            config,
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

    /// Append a resource at the end of the queue (or after a specific index).
    pub fn insert(&self, resource: Resource, after_index: Option<usize>) {
        let mut items = self.items.lock_sync();
        let pos = after_index.map_or(items.len(), |i| (i + 1).min(items.len()));
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
        let rate = self.default_rate();
        self.rate.store(rate, Ordering::Relaxed);

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
        let _ = self.send_to_slot(PlayerCmd::SetPaused(false));

        self.set_status(PlayerStatus::ReadyToPlay);
        let _ = self.events_tx.send(PlayerEvent::RateChanged { rate });
        debug!(rate, "play");
    }

    /// Pause playback (sets rate to 0.0).
    pub fn pause(&self) {
        self.rate.store(0.0, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
        let _ = self.events_tx.send(PlayerEvent::RateChanged { rate: 0.0 });
        debug!("pause");
    }

    /// Current playback rate (0.0 = paused).
    pub fn rate(&self) -> f32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Set playback rate.
    pub fn set_rate(&self, rate: f32) {
        self.rate.store(rate, Ordering::Relaxed);
        let _ = self.events_tx.send(PlayerEvent::RateChanged { rate });
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
    pub fn set_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume.store(clamped, Ordering::Relaxed);
        self.engine.set_master_volume(clamped);
        let _ = self
            .events_tx
            .send(PlayerEvent::VolumeChanged { volume: clamped });
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
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
        let _ = self.events_tx.send(PlayerEvent::MuteChanged { muted });
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
            let _ = self.events_tx.send(PlayerEvent::CurrentItemChanged);
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
    pub fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events_tx.subscribe()
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

    /// Number of EQ bands available for this player.
    pub fn eq_band_count(&self) -> usize {
        self.config.eq_bands
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

    /// Select and load a queue item by index.
    pub fn select_item(&self, index: usize, autoplay: bool) -> Result<(), PlayError> {
        let items_len = self.item_count();
        if index >= items_len {
            return Err(PlayError::Internal(format!(
                "item index out of range: {index} (len: {items_len})"
            )));
        }

        self.ensure_engine_started()?;
        self.ensure_slot()?;
        self.current_index.store(index, Ordering::Relaxed);
        let _ = self.events_tx.send(PlayerEvent::CurrentItemChanged);

        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(self.crossfade_duration()));
        self.load_current_item();

        if autoplay {
            let default_rate = self.default_rate();
            self.rate.store(default_rate, Ordering::Relaxed);
            let _ = self.send_to_slot(PlayerCmd::SetPaused(false));
            let _ = self
                .events_tx
                .send(PlayerEvent::RateChanged { rate: default_rate });
            self.set_status(PlayerStatus::ReadyToPlay);
        } else {
            self.rate.store(0.0, Ordering::Relaxed);
            let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
            let _ = self.events_tx.send(PlayerEvent::RateChanged { rate: 0.0 });
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
            let _ = self
                .events_tx
                .send(PlayerEvent::StatusChanged { status: new_status });
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
        self.engine.set_slot_volume(id, 1.0)?;
        Ok(id)
    }

    /// Send a command to the current slot's processor.
    fn send_to_slot(&self, cmd: PlayerCmd) -> Result<(), PlayError> {
        let slot_id = (*self.current_slot.lock_sync())
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        self.engine.send_slot_cmd(slot_id, cmd)
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

#[cfg(test)]
mod tests {
    use kithara_audio::mock::TestPcmReader;
    use kithara_decode::PcmSpec;
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
        InsertAfterIndex,
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
        assert!((player.engine().master_volume() - 1.0).abs() < f32::EPSILON);
        player.set_volume(-1.0);
        assert!((player.volume() - 0.0).abs() < f32::EPSILON);
        assert!((player.engine().master_volume() - 0.0).abs() < f32::EPSILON);
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
            crossfade_duration: 2.0,
            default_rate: 0.5,
            eq_bands: 5,
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
            .with_eq_bands(5);
        assert_eq!(config.max_slots, 8);
        assert!((config.default_rate - 0.5).abs() < f32::EPSILON);
        assert!((config.crossfade_duration - 2.5).abs() < f32::EPSILON);
        assert_eq!(config.eq_bands, 5);
    }

    #[kithara::test(tokio)]
    #[case(InsertScenario::AppendTwice, 2)]
    #[case(InsertScenario::InsertAfterIndex, 3)]
    async fn player_insert_scenarios(
        #[case] scenario: InsertScenario,
        #[case] expected_count: usize,
    ) {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        if matches!(scenario, InsertScenario::InsertAfterIndex) {
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
        assert!(matches!(event, Ok(PlayerEvent::CurrentItemChanged)));
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
        assert!(matches!(e1, Ok(PlayerEvent::VolumeChanged { .. })));
        assert!(matches!(e2, Ok(PlayerEvent::MuteChanged { .. })));
        assert!(matches!(e3, Ok(PlayerEvent::RateChanged { .. })));
    }

    #[kithara::test(tokio)]
    async fn player_negative_crossfade_duration_clamped() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_crossfade_duration(-5.0);
        assert!((player.crossfade_duration() - 0.0).abs() < f32::EPSILON);
    }
}
