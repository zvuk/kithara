use std::{num::NonZeroUsize, sync::Arc};

use kithara_events::{DownloaderEvent, Event, TrackId, TrackStatus};
use kithara_play::{PlayerImpl, Resource, ResourceConfig};
use tokio::{spawn, sync::Semaphore, task::JoinHandle};
use tracing::{debug, warn};

use crate::{
    error::QueueError,
    track::{TrackSource, Tracks},
};

/// Async track loader: parallelism-capped `ResourceConfig` -> `Resource`.
///
/// Owns the semaphore that limits concurrent `Resource::new` invocations and
/// subscribes to the per-resource [`EventBus`](kithara_events::EventBus) to
/// surface `LoadSlow` as [`QueueEvent::TrackStatusChanged`] with
/// [`TrackStatus::Slow`]. On failure emits `Failed(reason)`; successful
/// resources are returned to [`Queue`](crate::Queue) for `replace_item` +
/// `Loaded` emission.
pub(crate) struct Loader {
    player: Arc<PlayerImpl>,
    semaphore: Arc<Semaphore>,
    /// Same `Arc<Tracks>` as `Queue::tracks`. All status transitions go
    /// through [`Tracks::set_status`] so polled and event-stream views
    /// never drift.
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
            semaphore: Arc::new(Semaphore::new(max_concurrent_loads.get())),
            tracks,
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
        let mut config = match source {
            TrackSource::Uri(url) => ResourceConfig::new(&url)
                .map_err(|e| QueueError::InvalidUrl(format!("{url}: {e}")))?,
            TrackSource::Config(boxed) => *boxed,
        };
        self.player.prepare_config(&mut config);
        Ok(config)
    }

    /// Load a [`Resource`] for the given track. Caller is responsible for
    /// applying it via `PlayerImpl::replace_item` and emitting
    /// [`TrackStatus::Loaded`].
    pub(crate) async fn load(
        &self,
        id: TrackId,
        source: TrackSource,
    ) -> Result<Resource, QueueError> {
        let config = self.build_config(source)?;
        let bus_for_slow = config.bus.clone();
        let tracks = Arc::clone(&self.tracks);

        let slow_listener = spawn(async move {
            let Some(bus) = bus_for_slow else { return };
            let mut rx = bus.subscribe();
            while let Ok(ev) = rx.recv().await {
                if matches!(ev, Event::Downloader(DownloaderEvent::LoadSlow)) {
                    tracks.set_status(id, TrackStatus::Slow);
                    break;
                }
            }
        });

        let result = Resource::new(config).await;
        slow_listener.abort();

        result.map_err(|e| QueueError::Resource(format!("{e}")))
    }

    /// Spawn an async load. Acquires a semaphore permit for the duration of
    /// the load. Emits `Loading` on start, `Failed(reason)` on error. On
    /// success, the [`Resource`] is returned through the `JoinHandle`.
    pub(crate) fn spawn_load(
        self: &Arc<Self>,
        id: TrackId,
        source: TrackSource,
    ) -> JoinHandle<Result<Resource, QueueError>> {
        let this = Arc::clone(self);
        spawn(async move {
            let permit = Arc::clone(&this.semaphore)
                .acquire_owned()
                .await
                .map_err(|e| QueueError::Resource(format!("semaphore closed: {e}")))?;

            this.tracks.set_status(id, TrackStatus::Loading);

            let result = this.load(id, source).await;
            drop(permit);

            match &result {
                Ok(_) => debug!(id = id.as_u64(), "track load ok"),
                Err(e) => {
                    warn!(id = id.as_u64(), error = %e, "track load failed");
                    this.tracks
                        .set_status(id, TrackStatus::Failed(format!("{e}")));
                }
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use derivative::Derivative;
    use derive_setters::Setters;
    use kithara_events::{EventBus, QueueEvent};
    use kithara_play::PlayerConfig;
    use kithara_test_utils::kithara;
    use tokio::{
        spawn,
        time::{sleep as tokio_sleep, timeout as tokio_timeout},
    };

    use super::*;
    use crate::track::TrackEntry;

    const CAP_3: NonZeroUsize = match NonZeroUsize::new(3) {
        Some(n) => n,
        None => unreachable!(),
    };

    /// Builder for test [`Loader`] fixtures. Defaults cover most tests;
    /// override via setters when a specific concurrency cap matters.
    #[derive(Derivative, Setters)]
    #[derivative(Default)]
    #[setters(prefix = "with_")]
    struct LoaderBuilder {
        #[derivative(Default(value = "CAP_3"))]
        cap: NonZeroUsize,
    }

    /// Test fixture: the [`Loader`] under test, the shared
    /// [`Tracks`] store (so tests can seed entries), and the root
    /// [`EventBus`] (so tests can subscribe for assertions).
    struct LoaderFixture {
        loader: Arc<Loader>,
        tracks: Arc<Tracks>,
        bus: EventBus,
    }

    impl LoaderBuilder {
        fn build(self) -> LoaderFixture {
            let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
            let bus = player.bus().clone();
            let tracks = Arc::new(Tracks::new(bus.clone()));
            LoaderFixture {
                loader: Arc::new(Loader::new(player, self.cap, Arc::clone(&tracks))),
                tracks,
                bus,
            }
        }
    }

    #[kithara::test(tokio)]
    async fn build_config_preserves_caller_supplied_config() {
        let loader = LoaderBuilder::default().build().loader;
        let Ok(given) = ResourceConfig::new("https://example.com/a.mp3") else {
            panic!("valid url");
        };
        let given = given.with_preferred_peak_bitrate(321.0);
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
        let loader = LoaderBuilder::default().build().loader;
        let Err(err) = loader.build_config(TrackSource::Uri("not-a-url".into())) else {
            panic!("should reject relative path");
        };
        assert!(matches!(err, QueueError::InvalidUrl(_)));
    }

    #[kithara::test(tokio)]
    async fn load_invalid_uri_returns_invalid_url_error() {
        let loader = LoaderBuilder::default().build().loader;
        let Err(err) = loader
            .load(TrackId(0), TrackSource::Uri("not-a-url".into()))
            .await
        else {
            panic!("should reject relative path");
        };
        assert!(matches!(err, QueueError::InvalidUrl(_)));
    }

    #[kithara::test(tokio, multi_thread)]
    async fn semaphore_caps_concurrent_loads() {
        let cap = NonZeroUsize::new(2).expect("2 > 0");
        let loader = LoaderBuilder::default().with_cap(cap).build().loader;

        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..6 {
            let sem = Arc::clone(&loader.semaphore);
            let in_flight = Arc::clone(&in_flight);
            let max_seen = Arc::clone(&max_seen);
            handles.push(spawn(async move {
                let _permit = sem.acquire_owned().await.expect("acquire");
                let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(cur, Ordering::SeqCst);
                tokio_sleep(Duration::from_millis(50)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.expect("joined");
        }
        assert!(
            max_seen.load(Ordering::SeqCst) <= 2,
            "concurrency exceeded cap: {}",
            max_seen.load(Ordering::SeqCst)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_load_bad_url_emits_failed_status() {
        let fx = LoaderBuilder::default().build();
        fx.tracks.lock().push(TrackEntry {
            id: TrackId(42),
            name: String::new(),
            url: None,
            status: TrackStatus::Pending,
        });
        let mut rx = fx.bus.subscribe();
        let loader = fx.loader;

        let handle = loader.spawn_load(TrackId(42), TrackSource::Uri("not-a-url".into()));
        let result = handle.await.expect("join");
        assert!(matches!(result, Err(QueueError::InvalidUrl(_))));

        let mut saw_loading = false;
        let mut saw_failed = false;
        for _ in 0..8 {
            match tokio_timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged {
                    id: TrackId(42),
                    status: TrackStatus::Loading,
                }))) => saw_loading = true,
                Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged {
                    id: TrackId(42),
                    status: TrackStatus::Failed(_),
                }))) => saw_failed = true,
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        assert!(saw_loading, "Loading status event missing");
        assert!(saw_failed, "Failed status event missing");
    }
}
