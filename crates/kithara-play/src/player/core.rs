use kithara_abr::{AbrController, AbrSettings};
use kithara_audio::{EngineLoad, StretchControls};
use kithara_decode::GaplessMode;
use kithara_platform::{
    CancelScope,
    sync::{Arc, Mutex},
};
use tracing::debug;

use super::{
    config::PlayerConfig,
    state::{PlayerParams, PlayerPhase, Playlist, QueuedResource},
};
use crate::{
    api::{PlayerEvent, PlayerStatus},
    bridge::PlayerCmd,
    engine::{EngineConfig, EngineImpl},
    error::PlayError,
    resource::Resource,
    rt::track::PlayerResource,
};

/// Phase-neutral state shared across every player phase.
///
/// Field order is drop order: `items` (holding undelivered resources that
/// carry worker references) drops before `engine`, and `engine` (whose
/// `Drop` shuts the worker down) drops last.
pub(crate) struct PlayerCore {
    /// Live shared cost meter of the audio engine (decode + effects).
    /// Constructed once and kept address-stable for the player's lifetime.
    pub(crate) engine_load: Arc<EngineLoad>,

    pub(crate) params: PlayerParams,
    pub(crate) timestretch: Arc<StretchControls>,
    pub(crate) gapless_mode: GaplessMode,
    /// Engine drops last — worker shutdown happens after all tracks
    /// unregister and after `items` releases their resources.
    pub(crate) engine: EngineImpl,
    /// Items drop before engine — Audio tracks unregister from worker
    /// while it is still alive.
    pub(crate) playlist: Mutex<Playlist>,
    /// Status kept explicit (not derived from phase): `set_status` emits
    /// `StatusChanged` only on change and its values are not 1:1 with phase.
    pub(crate) status: Mutex<PlayerStatus>,
}

/// Concrete Player implementation managing items queue.
///
/// Owns an [`EngineImpl`] and sends commands to the active slot's processor.
/// When `play()` is called, the engine is lazily started and a slot is
/// allocated. The current queue item is taken out of the queue, wrapped in
/// [`PlayerResource`](crate::rt::track::PlayerResource), and sent
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
    pub(crate) const MIN_PLAYBACK_RATE: f32 = PlayerParams::MIN_PLAYBACK_RATE;

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

        let engine_config = EngineConfig::builder()
            .eq_layout(config.eq_layout.clone())
            .max_slots(config.max_slots)
            .sample_rate(config.sample_rate)
            .pcm_pool(resolved_pool.clone())
            .maybe_session(config.session.clone())
            .cancel(cancel.clone())
            .build();
        let engine = EngineImpl::new(engine_config, bus.clone());
        if config.abr.is_none() {
            config.abr = Some(AbrController::new(AbrSettings::default(), cancel.child()));
        }

        // Seed the single speed source with the configured default rate.
        config.timestretch.set_speed(config.default_rate);
        let core = PlayerCore {
            engine_load: Arc::new(EngineLoad::default()),
            params: PlayerParams::from(&config),
            timestretch: config.timestretch,
            gapless_mode: config.gapless_mode,
            status: Mutex::default(),
            playlist: Mutex::default(),
            engine,
        };
        Self {
            core,
            phase: Mutex::new(PlayerPhase::Idle),
        }
    }

    /// Advance to the next item in the queue.
    ///
    /// Does nothing if the current item is already the last one.
    pub fn advance_to_next_item(&self) {
        let mut playlist = self.core.playlist.lock();
        if let Some(index) = playlist.advance() {
            drop(playlist);
            self.announce_current_item(index);
            debug!(new_index = index, "advanced to next item");
        }
    }

    /// Sole publisher of `CurrentItemChanged`: emits only when `index` differs
    /// from the last announced item, so a `play()` resume of the same item
    /// stays quiet.
    pub(crate) fn announce_current_item(&self, index: usize) {
        if self.core.playlist.lock().mark_announced(index) {
            self.core
                .engine
                .bus()
                .publish(PlayerEvent::CurrentItemChanged);
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
        let mut playlist = self.core.playlist.lock();
        if index < playlist.len() {
            playlist.clear_item(index);
            drop(playlist);
            debug!(index, "item cleared");
        }
    }

    /// Insert a resource with optional queue-item identity metadata at a
    /// specific position, or append to the end.
    pub fn insert(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        at_position: Option<usize>,
    ) {
        let count = {
            let mut playlist = self.core.playlist.lock();
            playlist.insert(QueuedResource { item_id, resource }, at_position);
            playlist.len()
        };
        let pos = at_position.map_or_else(|| count.saturating_sub(1), |i| i.min(count));
        debug!(count, pos, "item inserted");
    }

    pub(crate) fn enqueue_to_processor(&self, index: usize) -> Option<(Arc<str>, f64)> {
        let mut playlist = self.core.playlist.lock();
        if index >= playlist.len() {
            return None;
        }

        let queued = playlist.take(index)?;
        let (item_id, resource) = (queued.item_id, queued.resource);

        let duration_seconds = resource
            .duration()
            .map_or(0.0, |duration| duration.as_secs_f64());
        let abr_handle = resource.abr_handle();
        self.phase.lock().set_abr_handle(abr_handle);

        let current_rate = self.core.timestretch.speed();
        resource.set_playback_rate(current_rate);

        let host_sr = self.core.engine.master_sample_rate();
        if let Some(sr) = std::num::NonZeroU32::new(host_sr) {
            resource.set_host_sample_rate(sr);
        }

        let src = Arc::clone(resource.src());
        let returned_src = Arc::clone(&src);
        let player_resource = PlayerResource::new(resource, src, self.core.engine.pcm_pool());
        drop(playlist);

        let _ = self.send_to_slot(PlayerCmd::LoadTrack {
            item_id,
            resource: Box::new(player_resource),
        });

        Some((returned_src, duration_seconds))
    }

    /// Remove item at index. Returns the removed resource, or `None` if out of
    /// bounds or already consumed.
    pub fn remove_at(&self, index: usize) -> Option<Resource> {
        self.unarm_next();

        let mut playlist = self.core.playlist.lock();
        let removed = playlist.remove_at(index);
        let remaining = playlist.len();
        drop(playlist);
        debug!(index, remaining, "item removed");
        removed.map(|queued| queued.resource)
    }

    /// Remove all items from the queue.
    pub fn remove_all_items(&self) {
        self.unarm_next();
        self.core.playlist.lock().clear();
        self.set_status(PlayerStatus::Unknown);
        let _ = self.send_to_slot(PlayerCmd::Clear);
        self.enter_stopped();
        debug!("all items removed");
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
        let mut playlist = self.core.playlist.lock();
        if index < playlist.len() {
            playlist.replace(index, QueuedResource { item_id, resource });
            drop(playlist);
            debug!(index, "item replaced");
        }
    }

    /// Pre-allocate empty slots so `replace_item` can fill them by index.
    pub fn reserve_slots(&self, count: usize) {
        self.core.playlist.lock().reserve(count);
        debug!(count, "slots reserved");
    }

    /// Internal: set status and emit event if changed.
    pub(crate) fn set_status(&self, new_status: PlayerStatus) {
        let mut status = self.core.status.lock();
        if *status != new_status {
            *status = new_status;
            drop(status);
            self.core
                .engine
                .bus()
                .publish(PlayerEvent::StatusChanged { status: new_status });
        }
    }
}

impl Drop for PlayerImpl {
    fn drop(&mut self) {
        self.core.engine.cancel();
    }
}

impl crate::api::Equalizer for PlayerImpl {
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
    use kithara_audio::{StretchControls, generate_log_spaced_bands};
    use kithara_decode::GaplessMode;
    use kithara_events::Event;
    use kithara_platform::CancelToken;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{api::SlotId, bridge::PlayerCmd, session::testing};

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
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .gapless_mode(GaplessMode::Disabled)
                .build(),
        );
        let mut config = crate::resource::ResourceConfig::new("https://example.com/song.mp3")
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
        let mut rc = crate::resource::ResourceConfig::new("https://example.com/song.mp3")
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
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .cancel(parent_master.clone())
                .build(),
        );
        let mut rc = crate::resource::ResourceConfig::new("https://example.com/song.mp3")
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
        player.core.params.set_rate(1.0);
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
        let config = PlayerConfig::builder()
            .crossfade_duration(2.0)
            .prefetch_duration(5.0)
            .default_rate(0.5)
            .eq_layout(generate_log_spaced_bands(5))
            .gapless_mode(GaplessMode::MediaOnly)
            .max_slots(2)
            .sample_rate(44_100)
            .timestretch(StretchControls::new(1.0))
            .build();
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
        assert!((player.core.timestretch.speed() - 2.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn set_rate_clamps_invalid_values() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.set_rate(0.0);
        assert!(player.rate() >= 0.01);
        assert!(player.core.timestretch.speed() >= 0.01);

        player.set_rate(-1.0);
        assert!(player.rate() >= 0.01);
    }

    #[kithara::test]
    fn timestretch_is_address_stable_across_play_pause() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .session(testing::test_session())
                .build(),
        );
        let ptr_before = Arc::as_ptr(&player.core.timestretch);
        player.play();
        player.pause();
        player.play();
        let ptr_after = Arc::as_ptr(&player.core.timestretch);
        assert_eq!(
            ptr_before, ptr_after,
            "timestretch controls must stay address-stable across transitions"
        );
        player.worker().shutdown();
    }

    #[kithara::test]
    fn pause_from_idle_is_noop() {
        use super::super::state::phase::PlayerPhaseKind;

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
        // `playlist` carry worker references. The phase (slot/abr/pending) holds
        // no worker-registered track directly. This pins that constructing,
        // arming a phase, and dropping does not panic / UAF: `phase` and
        // `core.playlist` must drop before `core.engine`.
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
        let player = PlayerImpl::new(PlayerConfig::builder().auto_advance_enabled(false).build());
        assert!(!player.auto_advance_enabled());
    }
}
