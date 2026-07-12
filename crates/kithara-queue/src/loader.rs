use std::num::NonZeroUsize;

use kithara_events::{DownloaderEvent, Event, EventBus, TrackId, TrackStatus};
use kithara_platform::{
    CancelToken,
    sync::Arc,
    tokio,
    tokio::{
        sync::Semaphore,
        task::{JoinHandle, spawn},
    },
};
use kithara_play::{PlayerImpl, Resource, ResourceConfig};

use crate::{
    attempts::{LoadClass, Ticket},
    error::QueueError,
    track::{TrackSource, Tracks},
};

/// Async track loader: `ResourceConfig` -> `Resource`, run in two
/// isolated permit lanes with one abortable attempt per track.
pub(crate) struct Loader {
    player: Arc<PlayerImpl>,
    /// Background prefetch lane (`max_concurrent_loads` permits).
    prefetch_lane: Arc<Semaphore>,
    /// User-selection lane: one dedicated permit, isolated from prefetch.
    interactive_lane: Arc<Semaphore>,
    /// Same `Arc<Tracks>` as `Queue::tracks`: owns per-track status and the live attempt,
    /// so both change under one lock.
    tracks: Arc<Tracks>,
}

impl Loader {
    pub(crate) fn new(
        player: Arc<PlayerImpl>,
        max_concurrent_loads: NonZeroUsize,
        tracks: Arc<Tracks>,
    ) -> Self {
        Self {
            player,
            tracks,
            prefetch_lane: Arc::new(Semaphore::new(max_concurrent_loads.get())),
            interactive_lane: Arc::new(Semaphore::new(1)),
        }
    }

    /// Build a [`ResourceConfig`] for the given [`TrackSource`].
    ///
    /// - [`TrackSource::Uri`] uses [`ResourceConfig::new`] defaults.
    ///   Callers wanting custom net/store behavior build a configured
    ///   [`ResourceConfig`] and pass it via [`TrackSource::Config`].
    /// - [`TrackSource::Config`] is passed through untouched (DRM keys,
    ///   headers, format hints preserved).
    ///
    /// Both paths finish with `PlayerImpl::prepare_config` so worker /
    /// sample-rate / runtime / default bus are injected.
    pub(crate) fn build_config(&self, source: TrackSource) -> Result<ResourceConfig, QueueError> {
        let config = match source {
            TrackSource::Uri(url) => ResourceConfig::new(&url)
                .map_err(|e| QueueError::InvalidUrl(format!("{url}: {e}")))?,
            TrackSource::Config(boxed) => *boxed,
        };
        Ok(self.player.prepare_config(config))
    }

    /// Load a [`Resource`] from a prepared config. Caller is responsible
    /// for applying it via `PlayerImpl::replace_item` and emitting [`TrackStatus::Loaded`].
    async fn load(&self, id: TrackId, config: ResourceConfig) -> Result<Resource, QueueError> {
        let slow_watcher =
            Self::watch_for_slow_status(id, config.bus.clone(), Arc::clone(&self.tracks));
        let resource_fut = async {
            Resource::new(config)
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

    /// Spawn a fresh async load in the given lane. `None` when a live
    /// attempt already exists - one track never occupies two permits.
    pub(crate) fn spawn_load(
        self: &Arc<Self>,
        id: TrackId,
        source: TrackSource,
        class: LoadClass,
    ) -> Option<JoinHandle<Result<Resource, QueueError>>> {
        let (config, cancel) = match self.attempt_config(id, source) {
            Ok(pair) => pair,
            Err(err) => return Some(self.spawn_config_failure(id, err)),
        };
        let ticket = self.tracks.begin_attempt(id, cancel.clone())?;
        Some(self.spawn_attempt(ticket, config, cancel, class))
    }

    /// Move a track's pending load into the interactive lane.
    pub(crate) fn promote_load(
        self: &Arc<Self>,
        id: TrackId,
        source: TrackSource,
    ) -> Option<JoinHandle<Result<Resource, QueueError>>> {
        let (config, cancel) = match self.attempt_config(id, source) {
            Ok(pair) => pair,
            Err(err) => return Some(self.spawn_config_failure(id, err)),
        };
        let ticket = self.tracks.promote_attempt(id, cancel.clone())?;
        Some(self.spawn_attempt(ticket, config, cancel, LoadClass::Interactive))
    }

    fn attempt_config(
        &self,
        id: TrackId,
        source: TrackSource,
    ) -> Result<(ResourceConfig, CancelToken), QueueError> {
        let config = self.build_config(source)?;
        let Some(cancel) = config.cancel.clone() else {
            return Err(QueueError::Resource(format!(
                "track {id:?}: resource config missing per-track cancel"
            )));
        };
        Ok((config, cancel))
    }

    fn spawn_attempt(
        self: &Arc<Self>,
        ticket: Ticket,
        config: ResourceConfig,
        cancel: CancelToken,
        class: LoadClass,
    ) -> JoinHandle<Result<Resource, QueueError>> {
        let this = Arc::clone(self);
        spawn(async move {
            let id = ticket.id;
            let lane = match class {
                LoadClass::Interactive => &this.interactive_lane,
                LoadClass::Prefetch => &this.prefetch_lane,
            };
            let permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
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
                _ = cancel.cancelled() => Err(QueueError::Cancelled(id)),
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

    /// Watches the [`EventBus`] for the first
    /// [`DownloaderEvent::LoadSlow`] and flips the track status to
    /// [`TrackStatus::Slow`]. Returns a never-completing future:
    /// the caller `select!`s it against `Resource::new`, so the
    /// completion side always belongs to the resource future.
    async fn watch_for_slow_status(
        id: TrackId,
        bus: Option<EventBus>,
        tracks: Arc<Tracks>,
    ) -> std::convert::Infallible {
        let mut rx = match bus {
            Some(b) => b.subscribe(),
            None => return std::future::pending().await,
        };
        let mut marked = false;
        while let Ok(ev) = rx.recv().await {
            if !marked && matches!(ev, Event::Downloader(DownloaderEvent::LoadSlow { .. })) {
                tracks.set_status(id, TrackStatus::Slow);
                marked = true;
            }
        }
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_events::{EventBus, QueueEvent};
    use kithara_platform::time::Duration;
    use kithara_play::PlayerConfig;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::track::TrackRecord;

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

    /// Test fixture: the [`Loader`] under test, the shared
    /// [`Tracks`] store (so tests can seed entries), and the root
    /// [`EventBus`] (so tests can subscribe for assertions).
    struct LoaderFixture {
        loader: Arc<Loader>,
        tracks: Arc<Tracks>,
        bus: EventBus,
    }

    impl LoaderFixtureSpec {
        fn build(self) -> LoaderFixture {
            let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
            let bus = player.bus().clone();
            let tracks = Arc::new(Tracks::new(bus.clone()));
            let loader = Arc::new(Loader::new(player, self.cap, Arc::clone(&tracks)));
            LoaderFixture {
                loader,
                tracks,
                bus,
            }
        }
    }

    #[kithara::test(tokio)]
    async fn build_config_preserves_caller_supplied_config() {
        let loader = LoaderFixtureSpec::default().build().loader;
        let Ok(builder) = ResourceConfig::for_src("https://example.com/a.mp3") else {
            panic!("valid url");
        };
        let given = builder.preferred_peak_bitrate(321.0).build();
        let Ok(returned) = loader.build_config(TrackSource::Config(Box::new(given))) else {
            panic!("build_config should succeed");
        };
        assert!(
            (returned.preferred_peak_bitrate - 321.0).abs() < f64::EPSILON,
            "caller-set fields must be preserved"
        );
    }

    #[kithara::test(tokio)]
    async fn build_config_invalid_uri_errors() {
        let loader = LoaderFixtureSpec::default().build().loader;
        let Err(err) = loader.build_config(TrackSource::Uri("not-a-url".into())) else {
            panic!("should reject relative path");
        };
        assert!(matches!(err, QueueError::InvalidUrl(_)));
    }

    #[kithara::test(tokio, multi_thread)]
    async fn prefetch_lane_caps_concurrent_loads() {
        let cap = NonZeroUsize::new(2).expect("BUG: 2 > 0 is mathematically guaranteed");
        let loader = LoaderFixtureSpec::default().with_cap(cap).build().loader;

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
                Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged {
                    id: TrackId(42),
                    status: TrackStatus::Loading,
                }))) => panic!("invalid config must not emit Loading"),
                Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged {
                    id: TrackId(42),
                    status: TrackStatus::Failed(_),
                }))) => saw_failed = true,
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        assert!(saw_failed, "Failed status event missing");
    }
}
