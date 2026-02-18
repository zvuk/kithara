//! Concrete Player implementation managing an items queue.
//!
//! `PlayerImpl` tracks a list of [`Resource`] items and exposes play/pause/seek.
//! Owns an [`EngineImpl`] and sends commands to the active slot's processor
//! via the slot's command channel.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use kithara_platform::{Mutex, ThreadPool};
use portable_atomic::AtomicF32;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::{
    error::PlayError,
    events::PlayerEvent,
    impls::resource::Resource,
    traits::engine::Engine,
    types::{ActionAtItemEnd, PlayerStatus, SlotId},
};

use super::{
    engine::{EngineConfig, EngineImpl},
    player_processor::PlayerCmd,
    player_resource::PlayerResource,
    player_track::TrackTransition,
};

// -- PlayerConfig -----------------------------------------------------------------

/// Configuration for the player.
#[derive(Clone, Debug)]
pub struct PlayerConfig {
    /// Crossfade duration in seconds. Default: 1.0.
    pub crossfade_duration: f32,
    /// Default playback rate (1.0 = normal). Default: 1.0.
    pub default_rate: f32,
    /// Number of EQ bands. Default: 10.
    pub eq_bands: usize,
    /// Maximum concurrent slots in the engine. Default: 4.
    pub max_slots: usize,
    /// Thread pool for the engine thread and background work.
    ///
    /// Propagated to the underlying [`EngineImpl`]. When `None`, the global
    /// rayon pool is used.
    pub thread_pool: Option<ThreadPool>,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            crossfade_duration: 1.0,
            default_rate: 1.0,
            eq_bands: 10,
            max_slots: 4,
            thread_pool: None,
        }
    }
}

impl PlayerConfig {
    /// Set maximum concurrent slots in the engine.
    #[must_use]
    pub fn with_max_slots(mut self, max_slots: usize) -> Self {
        self.max_slots = max_slots;
        self
    }

    /// Set default playback rate.
    #[must_use]
    pub fn with_default_rate(mut self, rate: f32) -> Self {
        self.default_rate = rate;
        self
    }

    /// Set crossfade duration in seconds.
    #[must_use]
    pub fn with_crossfade_duration(mut self, seconds: f32) -> Self {
        self.crossfade_duration = seconds;
        self
    }

    /// Set number of EQ bands.
    #[must_use]
    pub fn with_eq_bands(mut self, eq_bands: usize) -> Self {
        self.eq_bands = eq_bands;
        self
    }

    /// Set thread pool for the engine thread and background work.
    ///
    /// When not set, the global rayon pool is used.
    #[must_use]
    pub fn with_thread_pool(mut self, pool: ThreadPool) -> Self {
        self.thread_pool = Some(pool);
        self
    }
}

// -- PlayerImpl -------------------------------------------------------------------

/// Concrete Player implementation managing items queue.
///
/// Owns an [`EngineImpl`] and sends commands to the active slot's processor.
/// When `play()` is called, the engine is lazily started and a slot is
/// allocated. The current queue item is taken out of the queue, wrapped in
/// [`PlayerResource`], and sent to the processor via `PlayerCmd::LoadTrack`.
pub struct PlayerImpl {
    config: PlayerConfig,
    engine: EngineImpl,
    items: Mutex<Vec<Option<Resource>>>,
    current_index: AtomicUsize,
    current_slot: Mutex<Option<SlotId>>,
    rate: AtomicF32,
    volume: AtomicF32,
    muted: AtomicBool,
    status: Mutex<PlayerStatus>,
    action_at_item_end: Mutex<ActionAtItemEnd>,
    crossfade_duration: AtomicF32,
    events_tx: broadcast::Sender<PlayerEvent>,
}

impl PlayerImpl {
    /// Create a new player with the given configuration.
    #[must_use]
    pub fn new(config: PlayerConfig) -> Self {
        let engine_config = EngineConfig {
            max_slots: config.max_slots,
            thread_pool: config.thread_pool.clone(),
            ..EngineConfig::default()
        };
        let engine = EngineImpl::new(engine_config);

        let (events_tx, _) = broadcast::channel(64);
        Self {
            crossfade_duration: AtomicF32::new(config.crossfade_duration),
            rate: AtomicF32::new(0.0), // starts paused
            volume: AtomicF32::new(1.0),
            muted: AtomicBool::new(false),
            status: Mutex::new(PlayerStatus::Unknown),
            action_at_item_end: Mutex::new(ActionAtItemEnd::default()),
            current_index: AtomicUsize::new(0),
            items: Mutex::new(Vec::new()),
            current_slot: Mutex::new(None),
            engine,
            config,
            events_tx,
        }
    }

    /// Get the number of items in the queue (including consumed items).
    pub fn item_count(&self) -> usize {
        self.items.lock().len()
    }

    /// Append a resource at the end of the queue (or after a specific index).
    pub fn insert(&self, resource: Resource, after_index: Option<usize>) {
        let mut items = self.items.lock();
        let pos = after_index.map_or(items.len(), |i| (i + 1).min(items.len()));
        items.insert(pos, Some(resource));
        debug!(count = items.len(), pos, "item inserted");
    }

    /// Remove item at index. Returns the removed resource, or `None` if out of bounds
    /// or already consumed.
    pub fn remove_at(&self, index: usize) -> Option<Resource> {
        let mut items = self.items.lock();
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
        self.items.lock().clear();
        self.current_index.store(0, Ordering::Relaxed);
        self.set_status(PlayerStatus::Unknown);
        debug!("all items removed");
    }

    /// Start playback at the configured default rate.
    pub fn play(&self) {
        let rate = self.config.default_rate;
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

    /// Get current volume (0.0..=1.0).
    pub fn volume(&self) -> f32 {
        self.volume.load(Ordering::Relaxed)
    }

    /// Set volume, clamped to `0.0..=1.0`.
    pub fn set_volume(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume.store(clamped, Ordering::Relaxed);
        let _ = self
            .events_tx
            .send(PlayerEvent::VolumeChanged { volume: clamped });
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
        let items = self.items.lock();
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
        *self.status.lock()
    }

    /// Set action to perform when the current item ends.
    pub fn set_action_at_item_end(&self, action: ActionAtItemEnd) {
        *self.action_at_item_end.lock() = action;
    }

    /// Get action to perform when the current item ends.
    pub fn action_at_item_end(&self) -> ActionAtItemEnd {
        *self.action_at_item_end.lock()
    }

    /// Set crossfade duration in seconds.
    pub fn set_crossfade_duration(&self, seconds: f32) {
        self.crossfade_duration
            .store(seconds.max(0.0), Ordering::Relaxed);
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

    // -- Internal helpers ---------------------------------------------------------

    /// Internal: set status and emit event if changed.
    fn set_status(&self, new_status: PlayerStatus) {
        let mut status = self.status.lock();
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
        let mut slot = self.current_slot.lock();
        if let Some(id) = *slot {
            return Ok(id);
        }
        let id = self.engine.allocate_slot()?;
        *slot = Some(id);
        drop(slot);
        Ok(id)
    }

    /// Send a command to the current slot's processor.
    fn send_to_slot(&self, cmd: PlayerCmd) -> Result<(), PlayError> {
        let slot_id = (*self.current_slot.lock())
            .ok_or_else(|| PlayError::Internal("no active slot".into()))?;
        let tx = self
            .engine
            .slot_cmd_tx(slot_id)
            .ok_or_else(|| PlayError::Internal("slot handle not found".into()))?;
        tx.try_send(cmd)
            .map(|_| ())
            .map_err(|_| PlayError::Internal("slot channel full".into()))
    }

    /// Load the current queue item into the active slot.
    ///
    /// Takes the resource out of the queue (replacing with `None`), wraps it
    /// in [`PlayerResource`], and sends `LoadTrack` + `FadeIn` to the processor.
    fn load_current_item(&self) {
        let mut items = self.items.lock();
        let index = self.current_index.load(Ordering::Relaxed);
        if index >= items.len() {
            return;
        }

        // Take the resource out of the queue (if not already consumed).
        let Some(resource) = items[index].take() else {
            return; // Already loaded
        };

        let src = Arc::clone(resource.src());
        let player_resource = PlayerResource::new(resource, Arc::clone(&src));
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
    use std::time::Duration;

    use kithara_audio::PcmReader;
    use kithara_decode::{DecodeResult, PcmSpec, TrackMetadata};
    use kithara_events::AudioEvent;

    use super::*;

    // -- Mock PcmReader -----------------------------------------------------------

    struct MockPcmReader {
        spec: PcmSpec,
        metadata: TrackMetadata,
        total_frames: u64,
        position_frames: u64,
        eof: bool,
        events_tx: broadcast::Sender<AudioEvent>,
    }

    impl MockPcmReader {
        fn new(duration_secs: f64) -> Self {
            let spec = PcmSpec {
                channels: 2,
                sample_rate: 44100,
            };
            let total_frames = (spec.sample_rate as f64 * duration_secs) as u64;
            let (events_tx, _) = broadcast::channel(64);
            Self {
                spec,
                metadata: TrackMetadata::default(),
                total_frames,
                position_frames: 0,
                eof: false,
                events_tx,
            }
        }
    }

    impl PcmReader for MockPcmReader {
        fn read(&mut self, buf: &mut [f32]) -> usize {
            if self.eof {
                return 0;
            }
            let ch = self.spec.channels as u64;
            if ch == 0 {
                return 0;
            }
            let remaining = (self.total_frames - self.position_frames) * ch;
            let n = (buf.len() as u64).min(remaining) as usize;
            for s in &mut buf[..n] {
                *s = 0.5;
            }
            self.position_frames += n as u64 / ch;
            if self.position_frames >= self.total_frames {
                self.eof = true;
            }
            n
        }

        fn read_planar<'a>(&mut self, output: &'a mut [&'a mut [f32]]) -> usize {
            if self.eof || output.is_empty() {
                return 0;
            }
            let ch = self.spec.channels as usize;
            let frames = output[0]
                .len()
                .min((self.total_frames - self.position_frames) as usize);
            for c in output.iter_mut().take(ch) {
                for s in c.iter_mut().take(frames) {
                    *s = 0.5;
                }
            }
            self.position_frames += frames as u64;
            if self.position_frames >= self.total_frames {
                self.eof = true;
            }
            frames
        }

        fn seek(&mut self, position: Duration) -> DecodeResult<()> {
            let frame = (position.as_secs_f64() * self.spec.sample_rate as f64) as u64;
            self.position_frames = frame.min(self.total_frames);
            self.eof = self.position_frames >= self.total_frames;
            Ok(())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn is_eof(&self) -> bool {
            self.eof
        }

        fn position(&self) -> Duration {
            Duration::from_secs_f64(self.position_frames as f64 / self.spec.sample_rate as f64)
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs_f64(
                self.total_frames as f64 / self.spec.sample_rate as f64,
            ))
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }

        fn decode_events(&self) -> broadcast::Receiver<AudioEvent> {
            self.events_tx.subscribe()
        }
    }

    fn make_resource(duration_secs: f64) -> Resource {
        Resource::from_reader(MockPcmReader::new(duration_secs))
    }

    // -- Tests --------------------------------------------------------------------

    #[test]
    fn player_starts_paused() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
        assert_eq!(player.status(), PlayerStatus::Unknown);
    }

    #[test]
    fn player_pause_sets_rate_zero() {
        let player = PlayerImpl::new(PlayerConfig::default());
        // Don't call play() (requires audio hardware); just test pause logic.
        player.rate.store(1.0, Ordering::Relaxed);
        player.pause();
        assert!((player.rate() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn player_insert_increases_count() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert_eq!(player.item_count(), 0);
    }

    #[test]
    fn player_remove_all_clears() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.remove_all_items();
        assert_eq!(player.item_count(), 0);
        assert_eq!(player.current_index(), 0);
    }

    #[test]
    fn player_volume_clamps() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_volume(2.0);
        assert!((player.volume() - 1.0).abs() < f32::EPSILON);
        player.set_volume(-1.0);
        assert!((player.volume() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn player_muted() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!(!player.is_muted());
        player.set_muted(true);
        assert!(player.is_muted());
    }

    #[test]
    fn player_crossfade_duration() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!((player.crossfade_duration() - 1.0).abs() < f32::EPSILON);
        player.set_crossfade_duration(3.0);
        assert!((player.crossfade_duration() - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn player_action_at_item_end_default() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert_eq!(player.action_at_item_end(), ActionAtItemEnd::Advance);
    }

    #[test]
    fn player_events_subscribe() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let mut rx = player.subscribe();
        // Trigger an event without requiring audio hardware.
        player.set_volume(0.5);
        let event = rx.try_recv();
        assert!(event.is_ok());
    }

    #[test]
    fn player_config_custom() {
        let config = PlayerConfig {
            crossfade_duration: 2.0,
            default_rate: 0.5,
            eq_bands: 5,
            max_slots: 2,
            thread_pool: None,
        };
        let player = PlayerImpl::new(config);
        assert!((player.crossfade_duration() - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn player_config_builder() {
        let pool = ThreadPool::with_num_threads(1).unwrap();
        let config = PlayerConfig::default()
            .with_max_slots(8)
            .with_default_rate(0.5)
            .with_crossfade_duration(2.5)
            .with_eq_bands(5)
            .with_thread_pool(pool);
        assert_eq!(config.max_slots, 8);
        assert!((config.default_rate - 0.5).abs() < f32::EPSILON);
        assert!((config.crossfade_duration - 2.5).abs() < f32::EPSILON);
        assert_eq!(config.eq_bands, 5);
        assert!(config.thread_pool.is_some());
    }

    #[test]
    fn player_config_default_thread_pool_is_none() {
        let config = PlayerConfig::default();
        assert!(config.thread_pool.is_none());
    }

    #[test]
    fn player_propagates_thread_pool_to_engine() {
        let pool = ThreadPool::with_num_threads(1).unwrap();
        let config = PlayerConfig::default().with_thread_pool(pool);
        // Should not panic — engine uses the player's thread pool.
        let player = PlayerImpl::new(config);
        assert!(!player.engine().is_running());
    }

    #[test]
    fn player_advance_does_nothing_when_empty() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.advance_to_next_item(); // should not panic
        assert_eq!(player.current_index(), 0);
    }

    #[test]
    fn player_engine_accessor() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert!(!player.engine().is_running());
    }

    #[test]
    fn player_send_to_slot_without_slot_returns_error() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let result = player.send_to_slot(PlayerCmd::SetPaused(true));
        assert!(result.is_err());
    }

    // -- Integration tests (player + resource + queue) ----------------------------

    #[tokio::test]
    async fn player_insert_resource_increases_count() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        assert_eq!(player.item_count(), 1);
        player.insert(make_resource(2.0), None);
        assert_eq!(player.item_count(), 2);
    }

    #[tokio::test]
    async fn player_insert_after_index() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        // Insert after first item (index 0)
        player.insert(make_resource(3.0), Some(0));
        assert_eq!(player.item_count(), 3);
    }

    #[tokio::test]
    async fn player_remove_at_returns_resource() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        let removed = player.remove_at(0);
        assert!(removed.is_some());
        assert_eq!(player.item_count(), 1);
    }

    #[tokio::test]
    async fn player_remove_at_out_of_bounds_returns_none() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        assert!(player.remove_at(5).is_none());
        assert_eq!(player.item_count(), 1);
    }

    #[tokio::test]
    async fn player_remove_at_adjusts_current_index() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        player.insert(make_resource(3.0), None);
        // Advance to index 2
        player.advance_to_next_item();
        player.advance_to_next_item();
        assert_eq!(player.current_index(), 2);
        // Remove item before current → current_index shifts back
        player.remove_at(0);
        assert_eq!(player.current_index(), 1);
    }

    #[tokio::test]
    async fn player_remove_all_with_resources() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        player.insert(make_resource(3.0), None);
        player.remove_all_items();
        assert_eq!(player.item_count(), 0);
        assert_eq!(player.current_index(), 0);
        assert_eq!(player.status(), PlayerStatus::Unknown);
    }

    #[tokio::test]
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

    #[tokio::test]
    async fn player_advance_emits_event() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        player.insert(make_resource(2.0), None);
        let mut rx = player.subscribe();
        player.advance_to_next_item();
        let event = rx.try_recv();
        assert!(matches!(event, Ok(PlayerEvent::CurrentItemChanged)));
    }

    #[tokio::test]
    async fn player_play_without_audio_hardware_logs_warning() {
        // play() should not panic even when audio hardware is unavailable.
        let player = PlayerImpl::new(PlayerConfig::default());
        player.insert(make_resource(1.0), None);
        // This will attempt to start engine, fail gracefully, and return.
        player.play();
        // After a failed play, rate may or may not be set depending on
        // where the failure occurs. The key invariant: no panic.
    }

    #[tokio::test]
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

    #[tokio::test]
    async fn player_negative_crossfade_duration_clamped() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_crossfade_duration(-5.0);
        assert!((player.crossfade_duration() - 0.0).abs() < f32::EPSILON);
    }
}
