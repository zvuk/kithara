use std::num::NonZeroUsize;

use kithara_assets::AssetStore;
use kithara_audio::AudioObserver;
use kithara_bufpool::HasPool;
use kithara_download::DownloaderEvent;
use kithara_events::{Envelope, EventBus, ScopeLabel, TrackId};
use kithara_platform::{
    CancelGroup, CancelToken,
    sync::Arc,
    tokio,
    tokio::{
        sync::Semaphore,
        task::{JoinHandle, spawn},
    },
};
use kithara_play::{Resource, ResourceConfig, ResourceSrc, player::PlayerControl};
use kithara_test_utils::kithara;

use crate::{
    attempts::{LoadClass, Ticket},
    error::QueueError,
    event::TrackStatus,
    track::{TrackSource, Tracks},
};

/// Async track loader: `ResourceConfig` -> `Resource`, run in two
/// isolated permit lanes with one abortable attempt per track.
pub(crate) struct Loader<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// User-selection lane: one dedicated permit, isolated from prefetch.
    interactive_lane: Arc<Semaphore>,
    /// Background prefetch lane (`max_concurrent_loads` permits).
    prefetch_lane: Arc<Semaphore>,
    /// Same `Arc<Tracks>` as `Queue::tracks`: owns per-track status and the live attempt,
    /// so both change under one lock.
    tracks: Arc<Tracks<S>>,
    store: AssetStore<S>,
    cancel: CancelToken,
    player: PlayerControl<S>,
}

impl<S> Loader<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) fn new(
        player: PlayerControl<S>,
        store: AssetStore<S>,
        max_concurrent_loads: NonZeroUsize,
        tracks: Arc<Tracks<S>>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            cancel,
            player,
            tracks,
            store,
            interactive_lane: Arc::new(Semaphore::new(1)),
            prefetch_lane: Arc::new(Semaphore::new(max_concurrent_loads.get())),
        }
    }

    /// Attach `observer` through `id`'s live-or-pending decoder relay.
    pub(crate) fn attach_observer<O: AudioObserver>(&self, id: TrackId, observer: O) {
        self.tracks.attach_observer(id, Box::new(observer));
    }

    fn attempt_config(
        &self,
        id: TrackId,
        source: TrackSource<S>,
    ) -> Result<(ResourceConfig<S>, CancelToken), QueueError> {
        let config = self.build_config(id, source)?;
        let Some(cancel) = config.cancel().cloned() else {
            return Err(QueueError::Resource(format!(
                "track {id:?}: resource config missing per-track cancel"
            )));
        };
        Ok((config, cancel))
    }

    /// Build a [`ResourceConfig`] for the given [`TrackSource`].
    ///
    /// - [`TrackSource::Uri`] uses the queue store and player pools; other
    ///   resource options keep their defaults. Callers wanting custom
    ///   behavior build a configured [`ResourceConfig`] and pass it via
    ///   [`TrackSource::Config`].
    /// - [`TrackSource::Config`] is passed through untouched (DRM keys,
    ///   headers, format hints preserved).
    ///
    /// Both paths finish with `PlayerImpl::prepare_config` so worker /
    /// sample-rate / runtime / default bus are injected.
    pub(crate) fn build_config(
        &self,
        id: TrackId,
        source: TrackSource<S>,
    ) -> Result<ResourceConfig<S>, QueueError> {
        let mut config = match source {
            TrackSource::Uri(url) => {
                let src = ResourceSrc::parse(&url)
                    .map_err(|e| QueueError::InvalidUrl(format!("{url}: {e}")))?;
                ResourceConfig::for_src(src)
                    .store(self.store.clone())
                    .build()
            }
            TrackSource::Config(boxed) => *boxed,
        };
        if config.bus().is_none() {
            config.set_bus(self.player.bus().scoped_labeled(ScopeLabel {
                track: Some(id),
                ..ScopeLabel::default()
            }));
        }
        self.player.prepare_config(config).map_err(QueueError::from)
    }

    /// Load a [`Resource`] from a prepared config, attaching the observer
    /// left for this track when there is one. Caller is responsible
    /// for applying it via `PlayerImpl::replace_item` and emitting [`TrackStatus::Loaded`].
    async fn load(&self, id: TrackId, config: ResourceConfig<S>) -> Result<Resource, QueueError> {
        let slow_watcher =
            Self::watch_for_slow_status(id, config.bus().cloned(), Arc::clone(&self.tracks));
        let observer = self.tracks.observer_relay(id);
        let resource_fut = async {
            Resource::new_observed(config, Box::new(observer))
                .await
                .map_err(|e| QueueError::Resource(format!("{e}")))
        };
        tokio::pin!(slow_watcher);
        tokio::select! {
            biased;
            result = resource_fut => result,
            never = &mut slow_watcher => match never {},
        }
    }

    /// Move a track's pending load into the interactive lane.
    pub(crate) fn promote_load(
        self: &Arc<Self>,
        id: TrackId,
        source: TrackSource<S>,
    ) -> Option<JoinHandle<Result<Resource, QueueError>>> {
        let (config, cancel) = match self.attempt_config(id, source) {
            Ok(pair) => pair,
            Err(err) => return Some(self.spawn_config_failure(id, err)),
        };
        let ticket = self.tracks.promote_attempt(id, cancel.clone())?;
        Some(self.spawn_attempt(ticket, config, cancel, LoadClass::Interactive))
    }

    fn spawn_attempt(
        self: &Arc<Self>,
        ticket: Ticket,
        config: ResourceConfig<S>,
        track_cancel: CancelToken,
        class: LoadClass,
    ) -> JoinHandle<Result<Resource, QueueError>> {
        let this = Arc::clone(self);
        spawn(async move {
            let id = ticket.id;
            let cancel = CancelGroup::new(vec![track_cancel.clone(), this.cancel.clone()]);
            let lane = match class {
                LoadClass::Interactive => &this.interactive_lane,
                LoadClass::Prefetch => &this.prefetch_lane,
            };
            kithara::probe_event!(admission_started, track_id = id.as_u64());
            let permit = tokio::select! {
                biased;
                _ = Self::wait_and_cancel_track(&cancel, &track_cancel) => {
                    this.tracks.finish_attempt(&ticket, None);
                    return Err(QueueError::Cancelled(id));
                }
                permit = Arc::clone(lane).acquire_owned() => permit
                    .map_err(|e| QueueError::Resource(format!("semaphore closed: {e}")))?,
            };
            if !this.tracks.mark_loading(&ticket) {
                drop(permit);
                return Err(QueueError::Cancelled(id));
            }

            let result = tokio::select! {
                biased;
                _ = Self::wait_and_cancel_track(&cancel, &track_cancel) =>
                    Err(QueueError::Cancelled(id)),
                result = this.load(id, config) => result,
            };
            drop(permit);

            let failure = match &result {
                Ok(_) | Err(QueueError::Cancelled(_)) => None,
                Err(e) => Some(format!("{e}")),
            };
            this.tracks.finish_attempt(&ticket, failure);
            result
        })
    }

    /// Wrap a synchronous config failure (e.g. invalid URI) in a resolved
    /// handle so callers keep one completion path. No lane, no permit.
    fn spawn_config_failure(
        &self,
        id: TrackId,
        err: QueueError,
    ) -> JoinHandle<Result<Resource, QueueError>> {
        let tracks = Arc::clone(&self.tracks);
        spawn(async move {
            tracks.set_status(id, TrackStatus::Failed(format!("{err}")));
            Err(err)
        })
    }

    /// Spawn a fresh async load in the given lane. `None` when a live
    /// attempt already exists - one track never occupies two permits.
    pub(crate) fn spawn_load(
        self: &Arc<Self>,
        id: TrackId,
        source: TrackSource<S>,
        class: LoadClass,
    ) -> Option<JoinHandle<Result<Resource, QueueError>>> {
        let (config, cancel) = match self.attempt_config(id, source) {
            Ok(pair) => pair,
            Err(err) => return Some(self.spawn_config_failure(id, err)),
        };
        let ticket = self.tracks.begin_attempt(id, cancel.clone())?;
        Some(self.spawn_attempt(ticket, config, cancel, class))
    }

    async fn wait_and_cancel_track(cancel: &CancelGroup, track_cancel: &CancelToken) {
        cancel.cancelled().await;
        track_cancel.cancel();
    }

    /// Watches the [`EventBus`] for the first
    /// [`DownloaderEvent::LoadSlow`] and flips the track status to
    /// [`TrackStatus::Slow`]. Returns a never-completing future:
    /// the caller `select!`s it against `Resource::new`, so the
    /// completion side always belongs to the resource future.
    async fn watch_for_slow_status(
        id: TrackId,
        bus: Option<EventBus>,
        tracks: Arc<Tracks<S>>,
    ) -> std::convert::Infallible {
        let mut rx = match bus {
            Some(b) => b.subscribe::<DownloaderEvent>(),
            None => return std::future::pending().await,
        };
        let mut marked = false;
        while let Ok(Envelope { event: ev, .. }) = rx.recv().await {
            if !marked && matches!(ev, DownloaderEvent::LoadSlow { .. }) {
                tracks.set_status(id, TrackStatus::Slow);
                marked = true;
            }
        }
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_events::EventBus;
    use kithara_platform::{time::Duration, tokio::sync::oneshot};
    use kithara_play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, player::PlayerControlSource,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        event::QueueEvent,
        test_pools::{TestPools, pools},
        track::TrackRecord,
    };

    struct CancelDropProbe {
        state: Arc<AtomicUsize>,
        cancel: CancelToken,
    }

    impl Drop for CancelDropProbe {
        fn drop(&mut self) {
            self.state
                .store(usize::from(self.cancel.is_cancelled()), Ordering::SeqCst);
        }
    }

    /// Builder for test [`Loader`] fixtures. Defaults cover most tests;
    /// override via setters when a specific concurrency cap matters.
    struct LoaderFixtureSpec {
        cap: NonZeroUsize,
    }

    impl Default for LoaderFixtureSpec {
        fn default() -> Self {
            const CAP_3: NonZeroUsize = match NonZeroUsize::new(3) {
                Some(n) => n,
                None => unreachable!(),
            };
            Self { cap: CAP_3 }
        }
    }

    impl LoaderFixtureSpec {
        #[must_use]
        fn with_cap(mut self, cap: NonZeroUsize) -> Self {
            self.cap = cap;
            self
        }
    }

    #[kithara::test(tokio)]
    async fn cancellation_precedes_in_flight_future_drop() {
        let owner = CancelToken::root();
        let queue_cancel = owner.child();
        let track_cancel = owner.child();
        let group = CancelGroup::new(vec![queue_cancel.clone(), track_cancel.clone()]);
        let state = Arc::new(AtomicUsize::new(0));
        let probe_state = Arc::clone(&state);
        let probe_cancel = track_cancel.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let in_flight = async move {
            let _probe = CancelDropProbe {
                cancel: probe_cancel,
                state: probe_state,
            };
            let _ = started_tx.send(());
            future::pending::<()>().await;
        };
        let canceller = spawn(async move {
            started_rx.await.expect("in-flight future must start");
            queue_cancel.cancel();
        });

        tokio::select! {
            biased;
            _ = Loader::<TestPools>::wait_and_cancel_track(&group, &track_cancel) => {}
            () = in_flight => panic!("in-flight future must stay pending"),
        }
        canceller.await.expect("canceller task must not panic");

        assert_eq!(state.load(Ordering::SeqCst), 1);
    }

    /// Test fixture: the [`Loader`] under test, the shared
    /// [`Tracks`] store (so tests can seed entries), and the root
    /// [`EventBus`] (so tests can subscribe for assertions).
    struct LoaderFixture {
        loader: Arc<Loader<TestPools>>,
        tracks: Arc<Tracks<TestPools>>,
        bus: EventBus,
        _player: PlayerImpl<TestPools>,
    }

    impl LoaderFixtureSpec {
        fn build(self) -> LoaderFixture {
            let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
            let player = PlayerImpl::new(
                PlayerConfig::builder()
                    .sample_rate(crate::queue::TEST_SAMPLE_RATE)
                    .worker(worker)
                    .session(crate::queue::test_session())
                    .build(),
            );
            let bus = player.bus().clone();
            let tracks = Arc::new(Tracks::new(bus.clone()));
            let store = AssetStore::builder(player.pools().clone()).build();
            let loader = Arc::new(Loader::new(
                player.control(),
                store,
                self.cap,
                Arc::clone(&tracks),
                CancelToken::root(),
            ));
            LoaderFixture {
                loader,
                tracks,
                bus,
                _player: player,
            }
        }
    }

    #[kithara::test(tokio)]
    async fn build_config_preserves_caller_supplied_config() {
        let fixture = LoaderFixtureSpec::default().build();
        let loader = &fixture.loader;
        let supplied_store = AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build();
        let Ok(src) = ResourceSrc::parse("https://example.com/a.mp3") else {
            panic!("valid url");
        };
        let given = ResourceConfig::for_src(src)
            .store(supplied_store.clone())
            .preferred_peak_bitrate(321.0)
            .build();
        let Ok(returned) = loader.build_config(TrackId(1), TrackSource::Config(Box::new(given)))
        else {
            panic!("build_config should succeed");
        };
        assert!(
            (returned.preferred_peak_bitrate() - 321.0).abs() < f64::EPSILON,
            "caller-set fields must be preserved"
        );
        assert!(returned.store().is_same(&supplied_store));
        assert!(!returned.store().is_same(&loader.store));
    }

    #[kithara::test(tokio)]
    async fn build_config_labels_default_bus_with_track_id() {
        let fixture = LoaderFixtureSpec::default().build();
        let mut rx = fixture.bus.subscribe::<QueueEvent>();
        let Ok(config) = fixture.loader.build_config(
            TrackId(42),
            TrackSource::Uri("https://example.com/a.mp3".into()),
        ) else {
            panic!("build_config should succeed");
        };
        let Some(bus) = config.bus() else {
            panic!("build_config must inject a per-track bus");
        };
        assert!(config.store().is_same(&fixture.loader.store));
        bus.publish(QueueEvent::QueueEnded);
        let Ok(envelope) = rx.try_recv() else {
            panic!("scoped publish must reach the root subscriber");
        };
        assert_eq!(envelope.meta.track, Some(TrackId(42)));
    }

    #[kithara::test(tokio)]
    async fn build_config_invalid_uri_errors() {
        let fixture = LoaderFixtureSpec::default().build();
        let loader = &fixture.loader;
        let Err(err) = loader.build_config(TrackId(1), TrackSource::Uri("not-a-url".into())) else {
            panic!("should reject relative path");
        };
        assert!(matches!(err, QueueError::InvalidUrl(_)));
    }

    #[kithara::test(tokio, multi_thread)]
    async fn prefetch_lane_caps_concurrent_loads() {
        let cap = NonZeroUsize::new(2).expect("BUG: 2 > 0 is mathematically guaranteed");
        let fixture = LoaderFixtureSpec::default().with_cap(cap).build();
        let loader = &fixture.loader;

        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..6 {
            let sem = Arc::clone(&loader.prefetch_lane);
            let in_flight = Arc::clone(&in_flight);
            let max_seen = Arc::clone(&max_seen);
            handles.push(spawn(async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .expect("BUG: semaphore not closed in test");
                let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(cur, Ordering::SeqCst);
                time::sleep(Duration::from_millis(50)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.expect("BUG: spawned task panicked");
        }
        assert!(
            max_seen.load(Ordering::SeqCst) <= 2,
            "concurrency exceeded cap: {}",
            max_seen.load(Ordering::SeqCst)
        );
    }

    #[kithara::test(tokio, multi_thread)]
    async fn spawn_load_bad_url_emits_failed_status() {
        let fx = LoaderFixtureSpec::default().build();
        fx.tracks.lock().push(TrackRecord::new(
            TrackId(42),
            String::new(),
            TrackSource::Uri("not-a-url".into()),
        ));
        let mut rx = fx.bus.subscribe();
        let loader = fx.loader;

        let handle = loader
            .spawn_load(
                TrackId(42),
                TrackSource::Uri("not-a-url".into()),
                LoadClass::Prefetch,
            )
            .expect("config failure still yields a completion handle");
        let result = handle.await.expect("BUG: spawned task panicked");
        assert!(matches!(result, Err(QueueError::InvalidUrl(_))));

        // Invalid config fails synchronously without ever loading: the
        // track goes straight to Failed, no fictional Loading first.
        let mut saw_failed = false;
        for _ in 0..8 {
            match time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(Envelope {
                    event:
                        QueueEvent::TrackStatusChanged {
                            id: TrackId(42),
                            status: TrackStatus::Loading,
                        },
                    ..
                })) => panic!("invalid config must not emit Loading"),
                Ok(Ok(Envelope {
                    event:
                        QueueEvent::TrackStatusChanged {
                            id: TrackId(42),
                            status: TrackStatus::Failed(_),
                        },
                    ..
                })) => saw_failed = true,
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        assert!(saw_failed, "Failed status event missing");
    }
}
