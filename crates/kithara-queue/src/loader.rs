use std::{error::Error as StdError, io::Error};

use kithara_assets::AssetStore;
use kithara_audio::AudioObserver;
use kithara_bufpool::HasPool;
use kithara_download::{Downloader, DownloaderEvent};
use kithara_events::{Envelope, EventReceiver, RecvError, ScopeLabel, TrackId};
use kithara_net::NetError;
use kithara_platform::{
    CancelToken,
    sync::Arc,
    tokio,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_on},
};
use kithara_play::{
    ArtifactLoadError, Cover, LoadRefusal, OutputSnapshot, ResourceConfig, ResourceLoad,
    ResourcePrep, ResourceSrc,
};
use tracing::{debug, warn};

use crate::{
    error::QueueError,
    loading::{LoadReport, TrackLoad},
    queue::{QueueCommand, QueuePostbox},
    track::TrackSource,
};

/// What the queue opens each track with: its sources become prepared
/// configs, and the tasks beside a load report a slow transfer or a cover to
/// the queue's mailbox. The open itself goes through the track to the
/// dispatcher of the deck's owner.
pub(crate) struct Loader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    prep: ResourcePrep<S>,
    store: AssetStore<S>,
    downloader: Option<Downloader>,
    /// Where the tasks beside each load run; `None` fails every load.
    runtime: Option<RuntimeHandle>,
    /// Where a task beside a load reports to the queue.
    postbox: QueuePostbox<S>,
}

impl<S> Loader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn new(
        prep: ResourcePrep<S>,
        store: AssetStore<S>,
        downloader: Option<Downloader>,
        runtime: Option<RuntimeHandle>,
        postbox: QueuePostbox<S>,
    ) -> Self {
        Self {
            prep,
            store,
            downloader,
            runtime,
            postbox,
        }
    }
}

impl<S> Loader<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Build a [`ResourceConfig`] for the given [`TrackSource`].
    ///
    /// - [`TrackSource::Uri`] uses the queue store and downloader; other options
    ///   keep their defaults. Callers wanting custom behavior build a
    ///   configured [`ResourceConfig`] and pass it via
    ///   [`TrackSource::Config`].
    /// - [`TrackSource::Config`] is passed through untouched (DRM keys,
    ///   headers, format hints preserved).
    ///
    /// Both paths finish with the deck's prepare, so the worker, the session
    /// output and the bus labelled with the track are injected.
    pub(crate) fn build_config(
        &self,
        id: TrackId,
        source: TrackSource<S>,
        output: &OutputSnapshot,
    ) -> Result<ResourceConfig<S>, QueueError> {
        let mut config = match source {
            TrackSource::Uri(url) => {
                let src = ResourceSrc::parse(&url)
                    .map_err(|e| QueueError::InvalidUrl(format!("{url}: {e}")))?;
                ResourceConfig::for_src(src)
                    .store(self.store.clone())
                    .maybe_downloader(self.downloader.clone())
                    .build()
            }
            TrackSource::Config(boxed) => *boxed,
        };
        if config.bus().is_none() {
            config.set_bus(self.prep.bus.scoped_labeled(ScopeLabel {
                track: Some(id),
                ..ScopeLabel::default()
            }));
        }
        self.prep.prepare(config, output).map_err(QueueError::from)
    }

    /// The open of `id` from `source`, its decoder reaching `observer`, and
    /// the load that owns its token. The cover read and the slow-transfer
    /// watch start beside it.
    pub(crate) fn start(
        &self,
        id: TrackId,
        source: TrackSource<S>,
        observer: Box<dyn AudioObserver>,
        output: &OutputSnapshot,
    ) -> Result<(ResourceLoad<S>, TrackLoad), QueueError> {
        let config = self.build_config(id, source, output)?;
        let load = TrackLoad::new(&config)?;
        let runtime = self.runtime.as_ref().ok_or(QueueError::NoRuntime)?;
        self.read_cover(runtime, id, &config, &load);
        self.watch_for_slow_transfer(runtime, id, &config, &load);
        Ok((ResourceLoad::new(config, observer), load))
    }

    /// Read the track's cover beside its audio, over the load's transport and
    /// the track's token, and report it to the queue, which places it while
    /// that token lives. The audio never waits for the cover, and a cover
    /// that never arrives leaves the load untouched.
    fn read_cover(
        &self,
        runtime: &RuntimeHandle,
        id: TrackId,
        config: &ResourceConfig<S>,
        load: &TrackLoad,
    ) {
        let Some(cover) = config.artwork().cloned() else {
            return;
        };
        let config = config.clone();
        let token = load.token().clone();
        let postbox = self.postbox.clone();
        drop(spawn_on(runtime, async move {
            match config.artifact_fetch().load::<Cover>(&cover).await {
                Ok(cover) => report(
                    &postbox,
                    LoadReport::Cover {
                        id,
                        load: token,
                        cover: Arc::new(cover.into()),
                    },
                ),
                Err(ArtifactLoadError::Cancelled { .. }) => {}
                Err(error) => warn!(?id, %error, "the track's cover never arrived"),
            }
        }));
    }

    /// Watch the load's bus for its first slow transfer. The watch
    /// subscribes here, before the load is sent, so no `LoadSlow` slips past
    /// it, and ends with the load.
    fn watch_for_slow_transfer(
        &self,
        runtime: &RuntimeHandle,
        id: TrackId,
        config: &ResourceConfig<S>,
        load: &TrackLoad,
    ) {
        let Some(bus) = config.bus() else {
            return;
        };
        drop(spawn_on(
            runtime,
            slow_transfer(
                id,
                load.watch().clone(),
                bus.subscribe(),
                self.postbox.clone(),
            ),
        ));
    }
}

/// Post `report` to the queue. A queue that is gone has no track left to
/// report on.
fn report<S>(postbox: &QueuePostbox<S>, report: LoadReport)
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    if postbox.post(QueueCommand::Load(report)).is_err() {
        debug!("the queue is gone: dropping a load's report");
    }
}

/// Report the first [`DownloaderEvent::LoadSlow`] `events` delivers to the
/// queue, which turns the track [`TrackStatus::Slow`](crate::TrackStatus::Slow),
/// unless `watch` ends first. A quiet bus is a load still in flight, so the
/// watch waits as long as the load lives. A `Lagged` bus dropped the oldest
/// envelopes and keeps delivering, so the watch survives the gap; only
/// `Closed` ends it.
async fn slow_transfer<S>(
    id: TrackId,
    watch: CancelToken,
    mut events: EventReceiver<DownloaderEvent>,
    postbox: QueuePostbox<S>,
) where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let slow = async {
        let mut reported = false;
        loop {
            match events.recv().await {
                Ok(Envelope {
                    event: DownloaderEvent::LoadSlow { .. },
                    ..
                }) => {
                    if !reported {
                        report(
                            &postbox,
                            LoadReport::Slow {
                                id,
                                watch: watch.clone(),
                            },
                        );
                        reported = true;
                    }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    };
    tokio::select! {
        biased;
        () = watch.cancelled() => {}
        () = slow => {},
    }
}

/// Whether the dispatcher's refusal of an open is worth asking for again as
/// it stands: only an open that failed on a network cause a later ask can
/// answer.
pub(crate) fn asks_again(refusal: &LoadRefusal) -> bool {
    match refusal {
        LoadRefusal::Open(error) => can_answer_later(error),
        LoadRefusal::Capacity { .. }
        | LoadRefusal::Cancelled
        | LoadRefusal::NoRuntime
        | LoadRefusal::Pool(_)
        | LoadRefusal::Source(_) => false,
    }
}

/// Retry only when the error's typed network cause can answer later.
/// Error chains, not messages or asynchronous bus events, provide the verdict.
/// Parser/codec failures and stalled transfers are never connectivity retries.
fn can_answer_later(error: &(dyn StdError + 'static)) -> bool {
    net_cause(error).is_some_and(NetError::can_answer_later)
}

/// The network failure behind a load error, if the load failed on the network at
/// all.
///
/// [`io::Error`] hides its payload from [`StdError::source`] — it reports the
/// payload's *own* source instead — so a plain chain walk steps straight over a
/// wrapped `NetError`. This looks inside one explicitly.
#[kithara_test_utils::kithara::hang_watchdog]
fn net_cause<'e>(error: &'e (dyn StdError + 'static)) -> Option<&'e NetError> {
    let mut current = Some(error);
    while let Some(err) = current {
        hang_tick!();
        if let Some(net) = err.downcast_ref::<NetError>() {
            hang_reset!();
            return Some(net);
        }
        current = err
            .downcast_ref::<Error>()
            .and_then(Error::get_ref)
            .map_or_else(|| err.source(), |payload| Some(payload));
    }
    hang_reset!();
    None
}
#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        num::{NonZeroU16, NonZeroU64},
        pin::pin,
        task::{Context, Waker},
    };

    use kithara_assets::StorageBackend;
    use kithara_audio::AudioObserverSlot;
    use kithara_command::{ChannelConfig, Mailbox, channel, mailbox};
    use kithara_download::RequestId;
    use kithara_events::EventBus;
    use kithara_platform::time::{Duration, WallInstant, sleep, timeout};
    use kithara_play::{ArtifactSource, PlayWorker, PlayWorkerConfig, mock};
    use kithara_render::{LaneProtocol, LaneStart, Open};
    use kithara_sync::SpeedCurve;
    use kithara_test_utils::{TestTempDir, kithara, temp_dir};
    use kithara_warp::StretchKind;

    use super::*;
    use crate::{
        QueueEvent, TrackStatus,
        test_pools::{TestPools, pools},
        track::{TrackRecord, Tracks},
    };

    fn fixture() -> (
        Loader<TestPools>,
        Mailbox<QueueCommand<TestPools>, QueueError>,
    ) {
        let (postbox, mailbox) = mailbox();
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        let store = AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build();
        (
            Loader::new(
                prep,
                store,
                None,
                RuntimeHandle::try_current().ok(),
                postbox,
            ),
            mailbox,
        )
    }

    #[kithara::test(tokio)]
    async fn build_config_preserves_caller_supplied_config() {
        let (loader, _mailbox) = fixture();
        let supplied_store = AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build();
        let given = ResourceConfig::for_src(
            ResourceSrc::parse("https://example.com/a.mp3").expect("valid url"),
        )
        .store(supplied_store.clone())
        .preferred_peak_bitrate(321.0)
        .build();
        let returned = loader
            .build_config(
                TrackId(1),
                TrackSource::Config(Box::new(given)),
                &mock::output(None).get(),
            )
            .expect("build_config should succeed");
        assert!(
            (returned.preferred_peak_bitrate() - 321.0).abs() < f64::EPSILON,
            "caller-set fields must be preserved"
        );
        assert!(returned.store().is_same(&supplied_store));
        assert!(!returned.store().is_same(&loader.store));
    }

    #[kithara::test(tokio)]
    async fn build_config_forwards_a_prepared_artifact() {
        let (loader, _mailbox) = fixture();
        let src = ResourceSrc::parse("https://example.com/a.mp3").expect("valid url");
        let grid = ResourceSrc::parse("https://example.com/a.grid").expect("valid artifact url");
        let given = ResourceConfig::for_src(src)
            .store(AssetStore::builder(pools()).build())
            .beat_grid(ArtifactSource::from(grid.clone()))
            .waveform(ArtifactSource::Value(Arc::default()))
            .build();
        let returned = loader
            .build_config(
                TrackId(7),
                TrackSource::Config(Box::new(given)),
                &mock::output(None).get(),
            )
            .expect("build_config should succeed");
        assert!(
            matches!(returned.beat_grid(), Some(ArtifactSource::Source(src)) if *src == grid),
            "a grid source must reach the resource untouched"
        );
        assert!(
            matches!(returned.waveform(), Some(ArtifactSource::Value(_))),
            "a caller-held waveform must reach the resource untouched"
        );
    }

    #[kithara::test(tokio)]
    async fn build_config_labels_default_bus_with_track_id() {
        let (loader, _mailbox) = fixture();
        let mut rx = loader.prep.bus.subscribe::<QueueEvent>();
        let config = loader
            .build_config(
                TrackId(42),
                TrackSource::Uri("https://example.com/a.mp3".into()),
                &mock::output(None).get(),
            )
            .expect("build_config should succeed");
        let bus = config
            .bus()
            .expect("build_config must inject a per-track bus");
        assert!(config.store().is_same(&loader.store));
        bus.publish(QueueEvent::QueueEnded);
        let envelope = rx
            .try_recv()
            .expect("scoped publish must reach the root subscriber");
        assert_eq!(envelope.meta.track, Some(TrackId(42)));
    }

    #[kithara::test(tokio)]
    async fn build_config_invalid_uri_errors() {
        let (loader, _mailbox) = fixture();
        let Err(err) = loader.build_config(
            TrackId(1),
            TrackSource::Uri("not-a-url".into()),
            &mock::output(None).get(),
        ) else {
            panic!("should reject relative path");
        };
        assert!(matches!(err, QueueError::InvalidUrl(_)));
    }

    #[kithara::test(tokio)]
    async fn cancellation_wakes_an_attempt_waiting_for_admission() {
        let (loader, _mailbox) = fixture();
        let id = TrackId::allocate();
        let source: TrackSource<TestPools> = "https://example.com/pending.mp3".into();
        let mut tracks: Tracks<TestPools> = Tracks::default();
        tracks.records_mut().push(TrackRecord::new(
            id,
            "pending".into(),
            source.clone(),
            kithara_play::Position::ZERO,
        ));
        let (item, load) = loader
            .start(
                id,
                source,
                Box::new(AudioObserverSlot::default().relay()),
                &mock::output(None).get(),
            )
            .expect("fresh track prepares one unadmitted open");
        tracks.begin_load(id, load);
        assert!(
            tracks
                .records()
                .iter()
                .any(|record| record.id == id && record.load.is_some())
        );
        tracks.cancel_loads();
        let result = open(item).await;
        assert!(matches!(result, Err(LoadRefusal::Cancelled)));
        assert_eq!(tracks.records()[0].id, id);
        assert_eq!(tracks.records()[0].status, TrackStatus::Cancelled);
    }

    #[kithara::test(tokio)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn a_slow_watch_survives_a_bus_that_dropped_a_burst() {
        hang_tick!();
        let capacity = 4;
        let bus = EventBus::new(capacity);
        let (postbox, mut mailbox) = mailbox::<QueueCommand<TestPools>, QueueError>();
        let id = TrackId::allocate();
        let mut tracks: Tracks<TestPools> = Tracks::default();
        tracks.records_mut().push(TrackRecord::new(
            id,
            "slow".into(),
            TrackSource::Uri("https://example.com/slow.mp3".into()),
            kithara_play::Position::ZERO,
        ));
        let cancel = CancelToken::never().child();
        let config = ResourceConfig::for_src(
            ResourceSrc::parse("https://example.com/slow.mp3").expect("valid URL"),
        )
        .store(AssetStore::builder(pools()).build())
        .cancel(cancel)
        .build();
        let load = TrackLoad::new(&config).expect("track token");
        let mut watch = pin!(slow_transfer(
            id,
            load.watch().clone(),
            bus.subscribe(),
            postbox
        ));
        tracks.begin_load(id, load);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            watch.as_mut().poll(&mut cx).is_pending(),
            "the watch must subscribe before the burst it has to survive"
        );
        let request_id = RequestId::new(NonZeroU64::MIN);
        for _ in 0..=capacity {
            hang_tick!();
            bus.publish(DownloaderEvent::RequestStarted {
                request_id,
                wait_in_queue: Duration::ZERO,
            });
            hang_reset!();
        }
        bus.publish(DownloaderEvent::LoadSlow {
            request_id,
            elapsed: Duration::ZERO,
        });
        assert!(
            watch.as_mut().poll(&mut cx).is_pending(),
            "the watch never completes: it ends only with the resource it races"
        );
        for post in mailbox.drain() {
            hang_tick!();
            let QueueCommand::Load(report) = post.command else {
                panic!("a slow watch reports a load");
            };
            tracks.apply_report(report);
            hang_reset!();
        }
        assert_eq!(
            tracks.records()[0].status,
            TrackStatus::Slow,
            "a dropped burst must not deafen the watch to the `LoadSlow` behind it"
        );
    }

    #[kithara::test(tokio)]
    async fn a_slow_watch_waits_out_a_quiet_bus() {
        let _budget = kithara_test_utils::hang::override_timeout(Duration::from_millis(20));
        let bus = EventBus::new(4);
        let (postbox, _mailbox) = mailbox::<QueueCommand<TestPools>, QueueError>();
        let load = CancelToken::never().child();
        let mut watch = pin!(slow_transfer(
            TrackId::allocate(),
            load.clone(),
            bus.subscribe(),
            postbox
        ));
        bus.publish(DownloaderEvent::LoadSlow {
            request_id: RequestId::new(NonZeroU64::MIN),
            elapsed: Duration::ZERO,
        });
        assert!(
            timeout(Duration::from_millis(60), watch.as_mut())
                .await
                .is_err(),
            "a quiet bus is a load still in flight"
        );
        load.cancel();
        watch.await;
    }

    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn open(item: ResourceLoad<TestPools>) -> Result<(), LoadRefusal> {
        let (_sender, inbox) = channel::<LaneProtocol>(ChannelConfig::builder().build());
        let mut opened = pin!(item.open(
            Duration::ZERO,
            LaneStart {
                speed: SpeedCurve::Constant(1.0),
                keylock: false,
                backend: StretchKind::default(),
            },
            inbox,
        ));
        loop {
            hang_tick!();
            if let Ok(result) = timeout(Duration::from_millis(10), opened.as_mut()).await {
                hang_reset!();
                return result.map(|_| ());
            }
        }
    }

    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn cover_report(
        mailbox: &mut Mailbox<QueueCommand<TestPools>, QueueError>,
        tracks: &mut Tracks<TestPools>,
        bus: &EventBus,
    ) -> TrackId {
        let deadline = WallInstant::now() + Duration::from_secs(2);
        loop {
            hang_tick!();
            for post in mailbox.drain() {
                let QueueCommand::Load(report) = post.command else {
                    panic!("cover task reports a load");
                };
                tracks.apply_report(report);
                for event in tracks.drain_events() {
                    hang_tick!();
                    let changed = match &event {
                        QueueEvent::TrackMetadataChanged { id } => Some(*id),
                        _ => None,
                    };
                    bus.publish(event);
                    hang_reset!();
                    if let Some(id) = changed {
                        hang_reset!();
                        return id;
                    }
                }
            }
            assert!(
                WallInstant::now() < deadline,
                "the current attempt's cover lands"
            );
            sleep(Duration::from_millis(1)).await;
        }
    }

    #[kithara::test(native, tokio)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn only_the_current_attempt_places_its_cover(temp_dir: TestTempDir) {
        let (loader, mut mailbox) = fixture();
        let id = TrackId::allocate();
        let audio = ResourceSrc::Path(temp_dir.path().join("missing.mp3"));
        let covered = |name: &str, cover: &[u8]| -> TrackSource<TestPools> {
            ResourceConfig::for_src(audio.clone())
                .store(loader.store.clone())
                .artwork(ResourceSrc::Path(temp_dir.write(name, cover)))
                .build()
                .into()
        };
        let superseded = covered("superseded.jpg", b"superseded cover");
        let current = covered("current.jpg", b"current cover");
        let mut tracks = Tracks::default();
        tracks.records_mut().push(TrackRecord::new(
            id,
            "covered".into(),
            current.clone(),
            kithara_play::Position::ZERO,
        ));
        let bus = loader.prep.bus.clone();
        let mut events = bus.subscribe::<QueueEvent>();
        let output = mock::output(None).get();
        let observer = || Box::new(AudioObserverSlot::default().relay()) as Box<dyn AudioObserver>;
        let (first, load) = loader
            .start(id, superseded, observer(), &output)
            .expect("fresh load");
        tracks.begin_load(id, load);
        tracks.set_status(id, TrackStatus::Cancelled);
        assert!(matches!(open(first).await, Err(LoadRefusal::Cancelled)));
        let (second, load) = loader
            .start(id, current, observer(), &output)
            .expect("replacement load");
        tracks.begin_load(id, load);
        drop(tracks.drain_events());
        let changed = cover_report(&mut mailbox, &mut tracks, &bus).await;
        assert_eq!(changed, id);
        assert!(open(second).await.is_err(), "the audio is missing");
        let entry = tracks.records()[0].entry();
        assert_eq!(
            entry.metadata().artwork.as_deref().map(Vec::as_slice),
            Some(b"current cover".as_slice())
        );
        let changed = events
            .try_recv()
            .expect("the current attempt's cover lands");
        assert!(
            matches!(changed.event, QueueEvent::TrackMetadataChanged { id: changed } if changed == id)
        );
        for post in mailbox.drain() {
            if let QueueCommand::Load(report) = post.command {
                tracks.apply_report(report);
            }
        }
        for event in tracks.drain_events() {
            hang_tick!();
            bus.publish(event);
            hang_reset!();
        }
        while let Ok(envelope) = events.try_recv() {
            hang_tick!();
            assert!(
                !matches!(envelope.event, QueueEvent::TrackMetadataChanged { .. }),
                "the superseded attempt's cover landed"
            );
            hang_reset!();
        }
    }

    /// A spent budget over a refusal keeps the resource askable: the load is
    /// repeated while the selection wants it, which is how a track chosen during
    /// an outage starts once connectivity returns.
    #[kithara::test]
    fn a_refused_host_can_answer_later() {
        let refused = NetError::RetryExhausted {
            max_retries: 3,
            source: Box::new(NetError::Status {
                status: NonZeroU16::new(503).expect("503 is not zero"),
                url: None,
                body: Some("network offline".to_string()),
            }),
        };
        // `io::Error` hides its payload from the source chain; the classifier looks inside.
        assert!(can_answer_later(&Error::other(refused)));
    }

    /// A vanished transport is the same answer: nothing was reached, so the whole
    /// load is worth asking for again.
    #[kithara::test]
    fn a_vanished_host_can_answer_later() {
        let gone = NetError::Network("connection closed".to_string());
        assert!(can_answer_later(&Error::other(gone)));
    }

    /// A transfer that established and then stopped delivering is the net layer's
    /// own verdict: repeating it would spin instead of telling the user, the
    /// contract `stalled_master_playlist_fails_load` pins.
    #[kithara::test]
    fn a_stalled_transfer_is_not_asked_again() {
        let stalled = NetError::RetryExhausted {
            max_retries: 1,
            source: Box::new(NetError::Timeout),
        };
        assert!(!can_answer_later(&Error::other(stalled)));
    }

    /// A missing resource answers the same however long one waits.
    #[kithara::test]
    fn a_missing_resource_is_not_asked_again() {
        let missing = NetError::Status {
            status: NonZeroU16::new(404).expect("404 is not zero"),
            url: None,
            body: None,
        };
        assert!(!can_answer_later(&Error::other(missing)));
    }

    /// A failure the network had no part in — an unparseable container, a codec
    /// the build does not carry — is not a connectivity question.
    #[kithara::test]
    fn a_failure_with_no_network_cause_is_not_asked_again() {
        let local = Error::other("unsupported container");
        assert!(!can_answer_later(&local));
    }

    /// A cancelled open is the track's own decision, not an outage.
    #[kithara::test]
    fn a_cancelled_open_is_not_asked_again() {
        assert!(!asks_again(&LoadRefusal::Cancelled));
    }

    #[kithara::test]
    fn a_terminal_source_failure_is_not_asked_again() {
        use kithara_audio::{DecodeErrorKind, TrackFailureKind};

        for failure in [
            TrackFailureKind::Decode {
                kind: DecodeErrorKind::Io,
            },
            TrackFailureKind::RecreateFailed { offset: 91 },
            TrackFailureKind::SourceCancelled,
            TrackFailureKind::ChannelClosed,
            TrackFailureKind::Render,
        ] {
            assert!(!asks_again(&LoadRefusal::Source(failure)));
        }
    }
}
