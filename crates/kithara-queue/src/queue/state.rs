#[cfg(any(test, feature = "probe"))]
use std::collections::HashMap;
use std::{
    ops::Deref,
    sync::{Mutex, PoisonError},
};

use kithara_assets::{AssetStore, StorageBackend};
use kithara_bufpool::HasPool;
use kithara_events::{EventBus, EventReceiver, TrackId};
use kithara_platform::{CancelScope, CancelToken, sync::Arc};
use kithara_play::{
    PlayError, PlayerImpl,
    player::{PlayerControl, PlayerControlSource},
};

use super::types::{
    AtomicCachedPosition, AtomicTrackId, CachedPosition, CrossfadeArm, SelectPhase,
};
use crate::{
    config::QueueConfig,
    loader::Loader,
    navigation::NavigationState,
    track::{TrackRecord, Tracks},
};

/// Test-only respawn resource cache. Aliased so the field declaration
/// stays free of the structural `Arc<Mutex<HashMap<…>>>` god-map
/// pattern (see `arch.no-arc-mutex-godmap`).
#[cfg(any(test, feature = "probe"))]
pub(super) type TestResources = HashMap<TrackId, kithara_play::Resource>;

/// AVQueuePlayer-analogue orchestration facade.
///
/// Owns a [`PlayerImpl`] and a private async track loader, plus
/// queue-level state (ordered tracks, navigation, pending-select).
/// Publishes [`QueueEvent`](kithara_events::QueueEvent) on the shared
/// [`EventBus`] alongside player / audio / hls / file events so
/// [`Queue::subscribe`] returns a single unified stream.
#[doc(hidden)]
pub struct QueueRuntime<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Serializes every state-changing command against terminal close.
    pub(super) admission: Mutex<()>,
    /// Authoritative playback position updated on every `tick`. Filters
    /// transient 0.0 blips the engine reports on pause/resume —
    /// downstream UIs should read from this field rather than polling
    /// the engine directly. Read/written lock-free as a typed
    /// [`CachedPosition`] — [`CachedPosition::Unknown`] before the first
    /// stable sample.
    pub(super) cached_position: AtomicCachedPosition,
    /// Tracks the id of the track whose crossfade-advance has already
    /// been armed during `tick()`. Prevents triggering the next-track
    /// select repeatedly as the remaining playtime keeps ticking below
    /// the crossfade threshold. Cleared on
    /// [`QueueEvent::CurrentTrackChanged`](kithara_events::QueueEvent::CurrentTrackChanged).
    ///
    /// Read/written lock-free as a typed [`CrossfadeArm`] from the tick
    /// loop and the engine event handler.
    pub(super) crossfade_armed_for: AtomicTrackId,
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
    pub(super) autoplay_target: AtomicTrackId,
    pub(super) loader: Arc<Loader<S>>,
    pub(super) navigation: Arc<Mutex<NavigationState>>,
    pub(super) pending_select: Arc<Mutex<SelectPhase>>,
    /// Serialises a selection-apply against a concurrent [`Queue::select`].
    /// A track's `spawn_apply_after_load` completion and a later `select`
    /// that supersedes it both mutate the same selection state (pending,
    /// current, navigation cursor, `TrackStatus::Cancelled`); without a
    /// single serialization point the completion can observe-not-cancelled
    /// then `select_item` *after* the superseding select committed, so the
    /// superseded track barges in. Held only across the synchronous apply
    /// critical section — never across an `.await`. See the crate `CONTEXT.md`
    /// "Selection serialization".
    pub(super) select_apply: Arc<Mutex<()>>,
    /// Test-only respawn resource cache. Populated by
    /// [`Queue::supply_test_resource_for_respawn`] and consumed by
    /// `select` when a `Consumed` / `Cancelled` / `Failed` track is
    /// re-selected. Lets harness tests exercise the respawn path
    /// without a real loader.
    #[cfg(any(test, feature = "probe"))]
    pub(super) test_resources: Arc<Mutex<TestResources>>,
    /// Sole owner of the `Vec<TrackRecord>` (status, source, and live
    /// load attempt per track). Shared with [`Loader`] through
    /// `Arc<Tracks>`; every status transition goes through
    /// [`Tracks::set_status`](crate::track::Tracks::set_status) so polling
    /// and the event stream stay in sync.
    pub(super) tracks: Arc<Tracks<S>>,
    pub(super) bus: EventBus,
    /// Subscription to the shared bus; drained in `tick()` to convert
    /// engine events into queue-level side-effects (auto-advance / current
    /// track change forwarding).
    pub(super) player_rx: Mutex<EventReceiver>,
    /// Master cancel token for queue-owned loader work.
    pub(super) shutdown: CancelToken,
}

/// Cloneable queue command capability without beat-grid identity or topology.
pub struct QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(super) player: PlayerControl<S>,
    runtime: Arc<QueueRuntime<S>>,
}

impl<S> Clone for QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            player: self.player.clone(),
            runtime: Arc::clone(&self.runtime),
        }
    }
}

/// AVQueuePlayer-analogue orchestration facade.
///
/// Owns the resident player and its canonical synchronization state. Runtime
/// commands are exposed through a separate cloneable [`QueueControl`].
pub struct Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(super) control: QueueControl<S>,
    pub(super) player: PlayerImpl<S>,
}

impl<S> Deref for QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Target = QueueRuntime<S>;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl<S> Deref for Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Target = QueueControl<S>;

    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

impl<S> Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Build a queue from a [`QueueConfig`].
    ///
    /// The queue takes ownership of the supplied [`PlayerImpl`]; all access to
    /// the decorated player then goes through this facade.
    #[must_use]
    pub fn new(config: QueueConfig<S>) -> Self {
        let QueueConfig {
            player,
            store,
            cancel: config_cancel,
            max_concurrent_loads,
            max_history_size,
            prefetch_duration,
            #[cfg(any(test, feature = "probe"))]
            should_autoplay,
            #[cfg(not(any(test, feature = "probe")))]
                should_autoplay: _,
        } = config;
        let cancel = CancelScope::new(config_cancel).token();
        let store = store.unwrap_or_else(|| {
            AssetStore::builder(player.pools().clone())
                .backend(StorageBackend::default())
                .cancel(cancel.child())
                .build()
        });
        player.set_auto_advance_enabled(false);
        player.set_prefetch_duration(prefetch_duration);
        let bus = player.bus().clone();
        let player_control = player.control();
        let tracks = Arc::new(Tracks::new(bus.clone()));
        let loader = Arc::new(Loader::new(
            player_control.clone(),
            store,
            max_concurrent_loads,
            Arc::clone(&tracks),
            cancel.child(),
        ));
        let player_rx = player.subscribe();
        let runtime = Arc::new(QueueRuntime {
            admission: Mutex::new(()),
            loader,
            tracks,
            bus,
            #[cfg(any(test, feature = "probe"))]
            should_autoplay,
            shutdown: cancel,
            navigation: Arc::new(Mutex::new(NavigationState::new(max_history_size))),
            pending_select: Arc::new(Mutex::new(SelectPhase::Idle)),
            select_apply: Arc::new(Mutex::new(())),
            #[cfg(any(test, feature = "probe"))]
            test_resources: Arc::new(Mutex::new(HashMap::new())),
            player_rx: Mutex::new(player_rx),
            crossfade_armed_for: AtomicTrackId::disarmed(),
            #[cfg(any(test, feature = "probe"))]
            autoplay_target: AtomicTrackId::disarmed(),
            cached_position: AtomicCachedPosition::unknown(),
        });
        Self {
            control: QueueControl {
                player: player_control,
                runtime,
            },
            player,
        }
    }
}

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) fn invalidate(&self) {
        self.shutdown.cancel();
    }

    /// Close the resident player, then irreversibly cancel queue-owned work.
    ///
    /// # Errors
    ///
    /// Returns the player detach failure without cancelling the queue token;
    /// the player control gate is reopened so the owner can retry.
    pub fn close(&self) -> Result<(), PlayError> {
        let _admission = self.lock_admission();
        self.player.close()?;
        self.shutdown.cancel();
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), PlayError> {
        if self.is_closed() {
            Err(PlayError::Closed)
        } else {
            Ok(())
        }
    }

    pub(in crate::queue) fn with_open<T>(
        &self,
        operation: impl FnOnce(&Self) -> T,
    ) -> Result<T, PlayError> {
        let _admission = self.lock_admission();
        self.ensure_open()?;
        Ok(operation(self))
    }

    pub(in crate::queue) fn with_open_result<T, E>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<PlayError>,
    {
        let _admission = self.lock_admission();
        self.ensure_open().map_err(E::from)?;
        operation(self)
    }

    pub(in crate::queue) fn command(&self, operation: impl FnOnce(&Self)) {
        let _ = self.with_open(operation);
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shutdown.is_cancelled() || self.player.is_closed()
    }

    pub(in crate::queue) fn lock_admission(&self) -> std::sync::MutexGuard<'_, ()> {
        self.admission
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

    /// Acquire the selection-apply serialization guard (see
    /// [`Self::select_apply`]). Taken before `tracks`/`pending_select`/
    /// `navigation`/`player` in both `select` and the
    /// `spawn_apply_after_load` completion, so the two cannot interleave.
    pub(in crate::queue) fn lock_select_apply(&self) -> std::sync::MutexGuard<'_, ()> {
        self.select_apply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    delegate::delegate! {
        to self.tracks {
            #[call(lock)]
            pub(super) fn lock_tracks(&self) -> std::sync::MutexGuard<'_, Vec<TrackRecord<S>>>;
            #[call(lock)]
            pub(super) fn lock_tracks_mut(&self) -> std::sync::MutexGuard<'_, Vec<TrackRecord<S>>>;
            pub(super) fn set_status(&self, id: TrackId, status: kithara_events::TrackStatus);
        }
        to self.crossfade_armed_for {
            #[call(load)]
            pub(super) fn read_armed_for(&self) -> CrossfadeArm;
            #[call(take_if_matches)]
            pub(super) fn take_armed_for_if_matches(&self, id: TrackId) -> bool;
            #[call(store)]
            pub(super) fn write_armed_for(&self, arm: CrossfadeArm);
        }
        to self.cached_position {
            #[call(load)]
            pub(super) fn read_cached_position(&self) -> CachedPosition;
            #[call(store)]
            pub(super) fn write_cached_position(&self, pos: CachedPosition);
        }
    }
}

impl<S> Drop for Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.control.invalidate();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use core::sync::atomic::{AtomicU64, Ordering};
    use std::{
        num::NonZeroU32,
        sync::mpsc::{self, RecvTimeoutError},
        thread,
    };

    use kithara_audio::ConsumerWakeMode;
    use kithara_events::{Envelope, Event, EventReceiver, QueueEvent};
    use kithara_platform::{
        sync::{Arc, Mutex},
        time::{Duration, Instant, timeout},
    };
    use kithara_play::{
        AllocatedSlot, BeatGrid, Cmd, NodeInputs, PlayError, PlayWorker, PlayWorkerConfig,
        PlayerConfig, Reply, SessionDispatcher, SessionDuckingMode, SessionSampleRate, SharedEq,
        SlotId, bridge::slot_channels,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{TestPools, pools};

    pub(crate) const TEST_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
        Some(sample_rate) => sample_rate,
        None => unreachable!(),
    };

    /// No queue test ever streams bytes, so the store is here to be wired, not
    /// to hold anything. The default backend would map a file under the shared
    /// temp root, which every parallel test process also owns and which Miri
    /// cannot map at all.
    pub(in crate::queue) fn make_store() -> AssetStore<TestPools> {
        AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build()
    }

    pub(in crate::queue) fn make_queue() -> Queue<TestPools> {
        Queue::new(queue_config())
    }

    struct TestSession {
        next_slot: AtomicU64,
        nodes: Mutex<Vec<NodeInputs>>,
    }

    impl SessionDispatcher<TestPools> for TestSession {
        fn exec(&self, cmd: Cmd<TestPools>) -> Result<Reply, PlayError> {
            let reply = match cmd {
                Cmd::RegisterPlayer { .. } => Reply::PlayerRegistered(1),
                Cmd::AllocateSlot { .. } => {
                    let slot = SlotId::new(self.next_slot.fetch_add(1, Ordering::Relaxed));
                    let (inputs, control) = slot_channels(SharedEq::new(10));
                    self.nodes.lock().push(inputs);
                    Reply::SlotAllocated(AllocatedSlot::new(control, slot))
                }
                Cmd::QuerySampleRate => Reply::SampleRate(SessionSampleRate::new(None, 44_100)),
                Cmd::QueryStreamShape => Reply::StreamShape(None),
                Cmd::SessionDucking => Reply::SessionDucking(SessionDuckingMode::Off),
                _ => Reply::Ok,
            };
            Ok(reply)
        }

        fn consumer_wake_mode(&self) -> ConsumerWakeMode {
            ConsumerWakeMode::RealtimeDeferred
        }
    }

    pub(crate) fn test_session() -> Arc<dyn SessionDispatcher<TestPools>> {
        Arc::new(TestSession {
            next_slot: AtomicU64::new(0),
            nodes: Mutex::default(),
        })
    }

    fn queue_config() -> QueueConfig<TestPools> {
        QueueConfig::builder()
            .player(player())
            .store(make_store())
            .build()
    }

    fn player() -> PlayerImpl<TestPools> {
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(TEST_SAMPLE_RATE)
                .worker(worker)
                .session(test_session())
                .build(),
        )
    }

    pub(in crate::queue) async fn wait_for_queue_event<F>(
        rx: &mut EventReceiver,
        mut matches: F,
        timeout_ms: u64,
    ) -> bool
    where
        F: FnMut(&QueueEvent) -> bool,
    {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match timeout(remaining, rx.recv()).await {
                Ok(Ok(Envelope {
                    event: Event::Queue(ev),
                    ..
                })) if matches(&ev) => return true,
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
    fn queue_preserves_the_resident_players_canonical_grid() {
        let player = player();
        let grid_id = player.id();
        let snapshot = player.snapshot();
        let queue = Queue::new(QueueConfig::builder().player(player).build());

        assert_eq!(queue.id(), grid_id);
        assert_eq!(queue.snapshot(), snapshot);
    }

    #[kithara::test]
    fn queue_control_rejects_mutation_after_close() {
        let queue = make_queue();
        let control = queue.control.clone();

        control.close().expect("unstarted fixture must close");

        assert!(control.runtime.shutdown.is_cancelled());
        assert!(matches!(
            control.append("https://example.com/a.mp3"),
            Err(crate::QueueError::Play(PlayError::Closed))
        ));
        assert!(queue.is_empty());
    }

    #[kithara::test]
    fn close_waits_for_an_admitted_queue_mutation() {
        let queue = make_queue();
        let mutation_control = queue.control.clone();
        let close_control = queue.control.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (mutation_tx, mutation_rx) = mpsc::channel();
        let mutation = thread::spawn(move || {
            let result = mutation_control.with_open(|_| {
                entered_tx.send(()).expect("test receiver remains alive");
                release_rx.recv().expect("test sender releases mutation");
            });
            mutation_tx
                .send(result)
                .expect("test receiver remains alive");
        });

        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("mutation must enter the queue admission gate");
        let (close_tx, close_rx) = mpsc::channel();
        let close = thread::spawn(move || {
            close_tx
                .send(close_control.close())
                .expect("test receiver remains alive");
        });

        assert!(
            matches!(
                close_rx.recv_timeout(Duration::from_millis(50)),
                Err(RecvTimeoutError::Timeout)
            ),
            "close must not overtake an admitted queue mutation"
        );
        release_tx.send(()).expect("mutation thread remains alive");
        mutation_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("mutation must complete after release")
            .expect("admitted mutation remains open");
        close_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("close must complete after the mutation")
            .expect("unstarted fixture must close");
        mutation.join().expect("mutation thread must not panic");
        close.join().expect("close thread must not panic");
        assert!(queue.is_closed());
    }

    /// `PlayerImpl::set_prefetch_duration` names the queue as the canonical
    /// owner of this knob, so what the queue's config says has to be what
    /// the player it drives runs with.
    #[kithara::test]
    fn the_configured_prefetch_lead_reaches_the_player() {
        let queue = Queue::new(
            QueueConfig::builder()
                .player(player())
                .store(make_store())
                .prefetch_duration(8.0)
                .build(),
        );

        assert!((queue.player.prefetch_duration() - 8.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn crossfade_arm_disarmed_after_construction() {
        let queue = make_queue();
        assert_eq!(queue.read_armed_for(), CrossfadeArm::Disarmed);
    }

    #[kithara::test]
    fn crossfade_arm_take_only_disarms_matching_track() {
        let queue = make_queue();
        queue.write_armed_for(CrossfadeArm::armed(TrackId(9)));
        assert!(!queue.take_armed_for_if_matches(TrackId(10)));
        assert_eq!(
            queue.read_armed_for(),
            CrossfadeArm::Armed {
                for_track: TrackId(9),
            }
        );
        assert!(queue.take_armed_for_if_matches(TrackId(9)));
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
