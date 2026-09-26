use std::{
    convert::Infallible,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara::{
    analysis::{
        AnalysisFingerprint, AnalysisProgress, AnalysisToken, BeatArtifact, BeatGridModel,
        BeatGridState, BeatSnapshot, BeatState, Bucket, GRID_SCHEMA_VERSION, GridBeat, RangeSet,
        RawBeatGrid, Waveform,
    },
    assets::StorageBackend,
    download::{Downloader, DownloaderConfig},
    events::TrackId,
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
        time::{Duration, sleep},
        tokio::{
            runtime::Handle,
            sync::{mpsc, oneshot, watch},
            task,
        },
    },
    play::{PlayWorkerConfig, PlayerConfig, PlayerImpl, policy::DomainKeyPolicy},
    prelude::{ArtifactSource, ResourceSrc},
    queue::QueueConfig,
    worker::{DispatcherConfig, TaskConfig, Worker, WorkerConfig},
};
use kithara_test_fixtures::{asset::Asset, assets};
use kithara_test_utils::off_thread::OffThread;
use url::Url;

use super::{Entry, Request, TrackArtifacts};
use crate::{
    config::{AppConfig, AppDrm},
    pools::{
        self, AppHost, AppQueue, AppQueueControl, AppResourceConfig, AppStore, AppTrackSource,
        AppWorker, Pools, PoolsSection,
    },
    sources::build_resource_config,
    state::UiState,
    wave_cache::{AnalysisPersistence, AnalysisTarget, persistence::AnalysisPersistenceConfig},
    waveform::TrackAnalysis,
};

pub(crate) fn chunk_seconds() -> NonZeroU32 {
    NonZeroU32::new(16).expect("fixture chunk duration is non-zero")
}

pub(crate) fn test_pools() -> Pools {
    pools::build(&PoolsSection::default()).expect("valid app pool policy")
}

pub(crate) fn axis() -> NonZeroU32 {
    NonZeroU32::new(44_100).expect("fixture rate is non-zero")
}

pub(crate) fn other_axis() -> NonZeroU32 {
    NonZeroU32::new(48_000).expect("fixture rate is non-zero")
}

pub(crate) fn fingerprint() -> AnalysisFingerprint {
    AnalysisFingerprint::new(None, Some("wave:test:v1"))
}

pub(crate) fn progress(analysis: TrackAnalysis) -> AnalysisProgress {
    AnalysisProgress::try_from(analysis).expect("settled fixture is valid progress")
}

/// The blob encodes version 1 followed by one bucket of three 0.5 band heights, where 0.5 is the
/// little-endian float bytes `0x3F000000`.
pub(crate) fn one_bucket_wave() -> Waveform {
    Waveform::try_from([1, 0, 0, 0, 0, 0, 0, 63, 0, 0, 0, 63, 0, 0, 0, 63].as_slice())
        .expect("hand-built blob is valid")
}

pub(crate) fn grid() -> BeatSnapshot {
    BeatSnapshot::new(
        BeatArtifact::new(
            128.0,
            vec![(0, Some(0.9)), (500, None)],
            vec![(0, Some(0.9))],
        ),
        BeatState::Final,
        Vec::new(),
    )
}

pub(crate) fn snapshot(
    token: AnalysisToken,
    revision: u64,
    covered: u64,
    fingerprint: AnalysisFingerprint,
    beat: Option<BeatSnapshot>,
) -> TrackAnalysis {
    let mut coverage = RangeSet::new();
    coverage.insert(0..covered);
    TrackAnalysis::builder()
        .token(token)
        .revision(revision)
        .source_sample_rate(axis())
        .extent(1_000)
        .settled(true)
        .coverage(coverage)
        .fingerprint(fingerprint)
        .waveform(one_bucket_wave())
        .maybe_beat(beat)
        .build()
}

/// A settled pass that produced beats and no waveform: the cache entry of a
/// track whose waveform came from somewhere else.
pub(crate) fn beats_only(fingerprint: AnalysisFingerprint) -> TrackAnalysis {
    let mut coverage = RangeSet::new();
    coverage.insert(0..1_000);
    TrackAnalysis::builder()
        .token("test-track".into())
        .revision(4)
        .source_sample_rate(axis())
        .extent(1_000)
        .settled(true)
        .coverage(coverage)
        .fingerprint(fingerprint)
        .beat(grid())
        .build()
}

pub(crate) fn analysis() -> TrackAnalysis {
    snapshot("test-track".into(), 1, 1_000, fingerprint(), None)
}

pub(crate) fn revision_of(revision: u64) -> TrackAnalysis {
    snapshot("test-track".into(), revision, 1_000, fingerprint(), None)
}

pub(crate) fn revision_held(rx: &watch::Receiver<Option<TrackArtifacts>>) -> Option<u64> {
    rx.borrow()
        .as_ref()
        .and_then(TrackArtifacts::analysis)
        .map(TrackAnalysis::revision)
}

pub(crate) fn queue() -> (AppHost, AppQueueControl) {
    let worker = AppWorker::new(PlayWorkerConfig::builder(test_pools()).build());
    let mut host =
        AppHost::new(HostConfig::offline(worker.pools().clone()).build()).expect("test host");
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .worker(worker)
            .sample_rate(host.requested_sample_rate())
            .build(),
    );
    let queue = AppQueue::new(QueueConfig::builder().player(player).build());
    let queue = host.insert(queue).expect("host accepts queue");
    let control = queue.control().clone();
    (host, control)
}

pub(crate) async fn queue_off() -> (OffThread<(AppHost, AppQueueControl)>, AppQueueControl) {
    queue_off_named("app-host").await
}

pub(crate) async fn queue_off_named(
    name: &'static str,
) -> (OffThread<(AppHost, AppQueueControl)>, AppQueueControl) {
    let host = OffThread::spawn(name, || Ok::<_, Infallible>(queue()))
        .await
        .expect("app host fixture is infallible");
    let control = host.call(|(_, control)| control.clone()).await;
    (host, control)
}

/// Appends a track from the host owner thread, as the app would.
pub(crate) async fn track(
    host: &OffThread<(AppHost, AppQueueControl)>,
    id: u64,
    url: &str,
) -> (TrackId, AppTrackSource) {
    let track_id = TrackId::from(id);
    let url = url.to_owned();
    host.call(move |(_, queue)| {
        queue
            .append_with_id(track_id, url)
            .expect("append test track");
        let source = queue.track_source(track_id).expect("track has a source");
        (track_id, source)
    })
    .await
}

/// A grid the caller hands over, already checked.
pub(crate) fn served_grid() -> BeatGridModel {
    BeatGridModel::try_from(RawBeatGrid {
        schema_version: GRID_SCHEMA_VERSION,
        model_id: "served".to_owned(),
        revision: 1,
        state: BeatGridState::Final,
        duration: Some(2.0),
        bpm: 120.0,
        beats: vec![
            GridBeat {
                at: 0.0,
                ordinal: 0,
                confidence: Some(0.9),
            },
            GridBeat {
                at: 0.5,
                ordinal: 1,
                confidence: Some(0.9),
            },
        ],
        downbeats: Vec::new(),
        meter: None,
    })
    .expect("the served fixture grid holds together")
}

/// A waveform the caller hands over.
pub(crate) fn served_waveform() -> Waveform {
    Waveform::try_from(vec![Bucket::new(0.25, 0.5, 0.75); 8]).expect("fixture bands are in range")
}

/// Append a track the caller opened with artifacts already in hand.
pub(crate) async fn track_prepared(
    host: &OffThread<(AppHost, AppQueueControl)>,
    id: u64,
    url: &str,
    app: &AppConfig,
    beat_grid: Option<BeatGridModel>,
    waveform: Option<Waveform>,
) -> (TrackId, AppTrackSource) {
    track_sourced(
        host,
        id,
        url,
        app,
        beat_grid.map(|grid| ArtifactSource::from(Arc::new(grid))),
        waveform.map(|wave| ArtifactSource::from(Arc::new(wave))),
    )
    .await
}

/// Append a track the caller opened with artifacts however it holds them: a
/// structure in hand, or a source their bytes are read from.
pub(crate) async fn track_sourced(
    host: &OffThread<(AppHost, AppQueueControl)>,
    id: u64,
    url: &str,
    app: &AppConfig,
    beat_grid: Option<ArtifactSource<BeatGridModel>>,
    waveform: Option<ArtifactSource<Waveform>>,
) -> (TrackId, AppTrackSource) {
    let track_id = TrackId::from(id);
    let src = ResourceSrc::parse(url).expect("fixture url parses");
    let config = AppResourceConfig::for_src(src)
        .downloader(app.downloader.clone())
        .worker(app.worker.clone())
        .store(app.store.clone())
        .audio(app.audio.clone())
        .hls(app.hls.clone())
        .file(app.file.clone())
        .maybe_beat_grid(beat_grid)
        .maybe_waveform(waveform)
        .build();
    host.call(move |(_, queue)| {
        queue
            .append_with_id(track_id, config)
            .expect("append prepared test track");
        let source = queue.track_source(track_id).expect("track has a source");
        (track_id, source)
    })
    .await
}

/// Write one artifact document into the scratch directory and point a source
/// at it, so a test exercises the very path a caller configures a URL on.
pub(crate) fn document(name: &str, bytes: &[u8]) -> ResourceSrc {
    let path = std::env::temp_dir().join(format!("kithara-app-artifact-{name}"));
    std::fs::write(&path, bytes).expect("fixture document is written");
    ResourceSrc::Path(path)
}

pub(crate) fn memory_store() -> AppStore {
    AppStore::builder(test_pools())
        .backend(StorageBackend::Memory)
        .build()
}

pub(crate) fn app_config(cancel: &CancelToken, store: AppStore) -> AppConfig {
    let pools = test_pools();
    let worker = AppWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    AppConfig::builder()
        .drm(AppDrm::new(DomainKeyPolicy::new(Vec::new())))
        .downloader(Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(
                NetOptions::builder().build(),
                pools,
                cancel.child(),
            ))
            .build(),
        ))
        .shutdown(cancel.child())
        .worker(worker)
        .store(store)
        .build()
}

pub(crate) fn persistence(cancel: &CancelToken, pools: Pools) -> AnalysisPersistence {
    let worker = Worker::new(
        WorkerConfig::new()
            .with_cancel(cancel.child())
            .with_runtime(Handle::current()),
    );
    AnalysisPersistence::new(AnalysisPersistenceConfig::new(
        worker,
        pools,
        NonZeroUsize::MIN,
        Duration::from_secs(u64::from(chunk_seconds().get())),
        DispatcherConfig::builder()
            .name("analysis-service-test")
            .build(),
        TaskConfig::new(),
    ))
    .expect("persistence fixture starts")
}

fn asset_url(asset: Asset) -> String {
    let path = asset.path().expect("fixture is stored on disk");
    assert!(path.is_file(), "fixture file exists: {}", path.display());
    Url::from_file_path(path)
        .expect("fixture path is absolute")
        .into()
}

#[kithara::fixture]
pub(crate) fn tone_mp3() -> String {
    asset_url(assets::sine_mp3_a440_2s())
}

#[kithara::fixture]
pub(crate) fn rhythm_a_mp3() -> String {
    asset_url(assets::rhythm_mp3_deck_a_120bpm_48k())
}

#[kithara::fixture]
pub(crate) fn rhythm_b_mp3() -> String {
    asset_url(assets::rhythm_mp3_deck_b_120bpm_48k())
}

#[kithara::fixture]
pub(crate) fn short_wav() -> String {
    asset_url(assets::sine_wav_a440_2s())
}

#[kithara::fixture]
pub(crate) fn long_wav() -> String {
    asset_url(assets::sine_wav_a440_12s())
}

pub(crate) async fn next_subscribe(
    requests: &mut mpsc::Receiver<Request>,
) -> (
    TrackId,
    oneshot::Sender<watch::Receiver<Option<TrackArtifacts>>>,
) {
    loop {
        match requests.recv().await {
            Some(Request::Subscribe {
                track_id, reply, ..
            }) => return (track_id, reply),
            Some(Request::Warm { .. }) => {}
            None => panic!("the deck subscribes"),
        }
    }
}

/// Polls and cadence of [`wait_for_revision`]: two virtual seconds, which is
/// a pipeline statement rather than a host budget.
const REVISION_POLLS: usize = 2_000;
const REVISION_POLL_INTERVAL: Duration = Duration::from_millis(1);

pub(crate) async fn answer_subscribe(
    requests: &mut mpsc::Receiver<Request>,
    expected: TrackId,
) -> watch::Sender<Option<TrackArtifacts>> {
    let (track_id, reply) = next_subscribe(requests).await;
    assert_eq!(track_id, expected, "for the track its queue holds");
    let (tx, rx) = watch::channel(None);
    assert!(reply.send(rx).is_ok(), "the deck waits for the reply");
    tx
}

/// Answers every subscription for `expected` from one publication channel,
/// the way the analysis service answers them from a track's entry.
///
/// A deck lets go of its receiver before it asks again, and it asks again
/// whenever its event stream lags or its engine restarts. A fixture that
/// answers once leaves the deck holding nothing from the next ask onwards:
/// it mirrors no further revision, and the pass that publishes sees no
/// receiver left. Serving every ask keeps the deck subscribed for as long
/// as the caller holds the sender.
pub(crate) async fn serve_subscribe(
    mut requests: mpsc::Receiver<Request>,
    expected: TrackId,
) -> (
    watch::Sender<Option<TrackArtifacts>>,
    watch::Receiver<usize>,
) {
    let tx = answer_subscribe(&mut requests, expected).await;
    let (served, asks) = watch::channel(1);
    let sender = tx.clone();
    task::spawn(async move {
        let mut count = 1usize;
        while let Some(request) = requests.recv().await {
            if let Request::Subscribe { reply, .. } = request {
                let _ = reply.send(sender.subscribe());
                count += 1;
                served.send_replace(count);
            }
        }
    });
    (tx, asks)
}

/// Wait until the fixture has answered `count` subscriptions.
pub(crate) async fn wait_for_asks(asks: &mut watch::Receiver<usize>, count: usize) {
    while *asks.borrow_and_update() < count {
        asks.changed().await.expect("the fixture serves the deck");
    }
}

/// Wait until the deck has taken a publication at or past `revision`.
///
/// The poll sleeps rather than yielding. A bare yield leaves this task ready
/// for ever, so the engine never sees every participant idle and never moves
/// the virtual clock; a deck parked on a timer then waits on a clock this loop
/// is holding still, and the budget expires on a publication that was only
/// ever one clock step away.
pub(crate) async fn wait_for_revision(state: &Mutex<UiState>, revision: u64) {
    for _ in 0..REVISION_POLLS {
        if state
            .lock()
            .analysis
            .as_ref()
            .and_then(TrackArtifacts::analysis)
            .map(TrackAnalysis::revision)
            == Some(revision)
        {
            return;
        }
        sleep(REVISION_POLL_INTERVAL).await;
    }
    panic!("revision {revision} never reached the deck");
}

pub(crate) fn entry(
    config: &AppConfig,
    queue: AppQueueControl,
    track_id: TrackId,
    source: AppTrackSource,
) -> Entry {
    let AppTrackSource::Uri(url) = source else {
        panic!("fixture tracks are appended by URL");
    };
    let config = build_resource_config(&url, config).expect("source yields a resource");
    let target = AnalysisTarget::for_config(&config).expect("source has an analysis target");
    Entry::new(target, config, queue, track_id)
}
