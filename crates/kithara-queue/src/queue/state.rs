use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use kithara_events::{EventBus, EventReceiver, TrackId};
use kithara_play::{PlayerConfig, PlayerImpl};
use tokio_util::sync::CancellationToken;

use super::types::{
    AtomicCachedPosition, AtomicTrackId, CachedPosition, CrossfadeArm, SelectPhase,
};
use crate::{
    config::QueueConfig,
    loader::Loader,
    navigation::NavigationState,
    track::{TrackEntry, TrackSource, Tracks},
};

/// Track-id keyed cache of original `TrackSource`s. Aliased so the
/// field declaration stays free of the structural
/// `Arc<Mutex<HashMap<…>>>` god-map pattern (see `arch.no-arc-mutex-godmap`).
pub(super) type TrackSources = HashMap<TrackId, TrackSource>;

/// Test-only respawn resource cache, same aliasing rationale as
/// [`TrackSources`].
#[cfg(any(test, feature = "probe"))]
pub(super) type TestResources = HashMap<TrackId, kithara_play::Resource>;

/// AVQueuePlayer-analogue orchestration facade.
///
/// Owns an `Arc<PlayerImpl>` and a private async track loader, plus
/// queue-level state (ordered tracks, navigation, pending-select).
/// Publishes [`QueueEvent`](kithara_events::QueueEvent) on the shared
/// [`EventBus`] alongside player / audio / hls / file events so
/// [`Queue::subscribe`] returns a single unified stream.
pub struct Queue {
    /// Authoritative playback position updated on every `tick`. Filters
    /// transient 0.0 blips the engine reports on pause/resume —
    /// downstream UIs should read from this field rather than polling
    /// the engine directly. Read/written lock-free as a typed
    /// [`CachedPosition`] — [`CachedPosition::Unknown`] before the first
    /// stable sample.
    pub(super) cached_position: Arc<AtomicCachedPosition>,
    /// Tracks the id of the track whose crossfade-advance has already
    /// been armed during `tick()`. Prevents triggering the next-track
    /// select repeatedly as the remaining playtime keeps ticking below
    /// the crossfade threshold. Cleared on
    /// [`QueueEvent::CurrentTrackChanged`](kithara_events::QueueEvent::CurrentTrackChanged).
    ///
    /// Read/written lock-free as a typed [`CrossfadeArm`] from the tick
    /// loop and the engine event handler.
    pub(super) crossfade_armed_for: Arc<AtomicTrackId>,
    /// Whether this queue auto-starts playback once the first registered
    /// track finishes loading. Configured via
    /// [`QueueConfig::should_autoplay`]. `false` means the user must
    /// call [`Queue::select`] manually.
    ///
    /// Currently consumed only by the test-utils harness — the
    /// production register/insert paths do not arm autoplay yet (see
    /// `register_for_test` / `complete_load_for_test`). Gated with the
    /// same `cfg` so the field carries no cost outside tests.
    #[cfg(any(test, feature = "probe"))]
    pub(super) should_autoplay: bool,
    /// First registered track id awaiting autoplay-on-load. Set when
    /// `autoplay = true` and the queue has no active selection;
    /// consumed when the matching id finishes loading.
    /// [`CrossfadeArm::Disarmed`] = no pending target.
    #[cfg(any(test, feature = "probe"))]
    pub(super) autoplay_target: Arc<AtomicTrackId>,
    pub(super) loader: Arc<Loader>,
    pub(super) navigation: Arc<Mutex<NavigationState>>,
    pub(super) pending_select: Arc<Mutex<SelectPhase>>,
    pub(super) player: Arc<PlayerImpl>,
    /// Kept alongside `tracks` so a `Consumed` track can be re-spawned
    /// on re-selection (user tapping a previously-played track). The
    /// original `TrackSource::Config` — including DRM keys and custom
    /// net/headers — is preserved; a bare URL wouldn't be enough to
    /// reconstruct a DRM-protected source.
    pub(super) sources: Arc<Mutex<TrackSources>>,
    /// Test-only respawn resource cache. Populated by
    /// [`Queue::supply_test_resource_for_respawn`] and consumed by
    /// `select` when a `Consumed` / `Cancelled` / `Failed` track is
    /// re-selected. Lets harness tests exercise the respawn path
    /// without a real loader.
    #[cfg(any(test, feature = "probe"))]
    pub(super) test_resources: Arc<Mutex<TestResources>>,
    /// Sole owner of the `Vec<TrackEntry>`. Shared with [`Loader`]
    /// through `Arc<Tracks>`; every status transition goes through
    /// [`Tracks::set_status`](crate::track::Tracks::set_status) so polling
    /// and the event stream stay in sync.
    pub(super) tracks: Arc<Tracks>,
    pub(super) bus: EventBus,
    /// Subscription to the shared bus; drained in `tick()` to convert
    /// engine events into queue-level side-effects (auto-advance / current
    /// track change forwarding).
    pub(super) player_rx: Mutex<EventReceiver>,
    /// Master cancel token for the queue. When the queue creates its
    /// own [`PlayerImpl`] (no caller-supplied player), this token is
    /// passed into `PlayerConfig.cancel` so the queue's `Drop` cascades
    /// shutdown to the player's subsystems. When a caller supplies a
    /// pre-built player, this token is independent — the caller owns
    /// the player's master directly.
    pub(super) cancel: CancellationToken,
}

impl Queue {
    /// Build a queue from a [`QueueConfig`].
    ///
    /// If `config.player` is `Some`, the caller-supplied
    /// [`PlayerImpl`] is used (caller retains ownership; must not
    /// mutate its item list directly — `play`/`pause`/`seek` OK;
    /// `replace_item`, `reserve_slots`, `select_item`, `remove_at` are
    /// Queue-owned). If `None`, a default player is built internally.
    ///
    /// Matches the project-wide pattern where config structs accept
    /// optional built instances (see
    /// [`ResourceConfig`](kithara_play::ResourceConfig)'s `worker` /
    /// `runtime` / `bus` fields).
    #[must_use]
    pub fn new(config: QueueConfig) -> Self {
        let QueueConfig {
            player,
            max_concurrent_loads,
            prefetch_duration: _,
            #[cfg(any(test, feature = "probe"))]
            should_autoplay,
            #[cfg(not(any(test, feature = "probe")))]
                should_autoplay: _,
        } = config;
        let cancel = CancellationToken::new(); // kithara:cancel:owner
        let player = player.unwrap_or_else(|| {
            let config = PlayerConfig::builder().cancel(cancel.clone()).build();
            Arc::new(PlayerImpl::new(config))
        });
        player.set_auto_advance_enabled(false);
        let bus = player.bus().clone();
        let tracks = Arc::new(Tracks::new(bus.clone()));
        let loader = Arc::new(Loader::new(
            Arc::clone(&player),
            max_concurrent_loads,
            Arc::clone(&tracks),
        ));
        let player_rx = player.subscribe();
        Self {
            player,
            loader,
            tracks,
            bus,
            #[cfg(any(test, feature = "probe"))]
            should_autoplay,
            cancel,
            navigation: Arc::new(Mutex::new(NavigationState::new())),
            pending_select: Arc::new(Mutex::new(SelectPhase::Idle)),
            sources: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(any(test, feature = "probe"))]
            test_resources: Arc::new(Mutex::new(HashMap::new())),
            player_rx: Mutex::new(player_rx),
            crossfade_armed_for: Arc::new(AtomicTrackId::disarmed()),
            #[cfg(any(test, feature = "probe"))]
            autoplay_target: Arc::new(AtomicTrackId::disarmed()),
            cached_position: Arc::new(AtomicCachedPosition::unknown()),
        }
    }

    pub(super) fn lock_navigation(&self) -> std::sync::MutexGuard<'_, NavigationState> {
        self.navigation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn lock_navigation_mut(&self) -> std::sync::MutexGuard<'_, NavigationState> {
        self.navigation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(in crate::queue) fn lock_pending_select_mut(
        &self,
    ) -> std::sync::MutexGuard<'_, SelectPhase> {
        self.pending_select
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn lock_tracks(&self) -> std::sync::MutexGuard<'_, Vec<TrackEntry>> {
        self.tracks.lock()
    }

    pub(super) fn lock_tracks_mut(&self) -> std::sync::MutexGuard<'_, Vec<TrackEntry>> {
        self.tracks.lock()
    }

    pub(super) fn read_armed_for(&self) -> CrossfadeArm {
        self.crossfade_armed_for.load()
    }

    pub(super) fn read_cached_position(&self) -> CachedPosition {
        self.cached_position.load()
    }

    pub(super) fn set_status(&self, id: TrackId, status: kithara_events::TrackStatus) {
        self.tracks.set_status(id, status);
    }

    pub(super) fn take_armed_for(&self) -> CrossfadeArm {
        self.crossfade_armed_for.take()
    }

    pub(super) fn write_armed_for(&self, arm: CrossfadeArm) {
        self.crossfade_armed_for.store(arm);
    }

    pub(super) fn write_cached_position(&self, pos: CachedPosition) {
        self.cached_position.store(pos);
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
pub(super) mod tests {
    use kithara_events::{Event, EventReceiver, QueueEvent};
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use tokio::time::{Instant as TokioInstant, timeout as tokio_timeout};

    use super::*;

    pub(in crate::queue) fn make_queue() -> Queue {
        Queue::new(QueueConfig::default())
    }

    pub(in crate::queue) async fn wait_for_queue_event<F>(
        rx: &mut EventReceiver,
        mut matches: F,
        timeout_ms: u64,
    ) -> bool
    where
        F: FnMut(&QueueEvent) -> bool,
    {
        let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(TokioInstant::now());
            if remaining.is_zero() {
                return false;
            }
            match tokio_timeout(remaining, rx.recv()).await {
                Ok(Ok(Event::Queue(ev))) if matches(&ev) => return true,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => return false,
            }
        }
    }

    #[kithara::test]
    fn queue_new_constructs_without_panic() {
        let _queue = make_queue();
    }

    #[kithara::test]
    fn crossfade_arm_disarmed_after_construction() {
        let queue = make_queue();
        assert_eq!(queue.read_armed_for(), CrossfadeArm::Disarmed);
    }

    #[kithara::test]
    fn crossfade_arm_take_round_trips_then_disarms() {
        let queue = make_queue();
        queue.write_armed_for(CrossfadeArm::armed(TrackId(9)));
        assert_eq!(
            queue.take_armed_for(),
            CrossfadeArm::Armed {
                for_track: TrackId(9),
            }
        );
        assert_eq!(queue.read_armed_for(), CrossfadeArm::Disarmed);
    }

    #[kithara::test]
    fn cached_position_unknown_after_construction() {
        let queue = make_queue();
        assert_eq!(Option::<f64>::from(queue.read_cached_position()), None);
    }

    #[kithara::test]
    fn cached_position_round_trips_through_queue() {
        let queue = make_queue();
        queue.write_cached_position(CachedPosition::known(12.5));
        assert_eq!(
            Option::<f64>::from(queue.read_cached_position()),
            Some(12.5)
        );
    }

    #[kithara::test]
    fn select_phase_idle_after_construction() {
        let queue = make_queue();
        assert!(matches!(
            *queue.lock_pending_select_mut(),
            SelectPhase::Idle
        ));
    }
}
