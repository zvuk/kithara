use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::Path,
};

use kithara::{
    analysis::{
        AnalysisFingerprint, AnalysisProgress, AnalysisToken, BeatArtifact, BeatSnapshot,
        BeatState, Coverage, FrameRange, Waveform,
    },
    assets::StorageBackend,
    events::TrackId,
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::Duration,
        tokio::{runtime::Handle, sync::watch},
    },
    play::{PlayWorkerConfig, PlayerConfig, PlayerImpl},
    queue::QueueConfig,
    stream::dl::{Downloader, DownloaderConfig},
    worker::{DispatcherConfig, TaskConfig, Worker, WorkerConfig},
};
use num_traits::cast::AsPrimitive;

use super::Entry;
use crate::{
    config::AppConfig,
    pools::{self, AppHost, AppQueue, AppQueueControl, AppStore, AppTrackSource, AppWorker, Pools},
    sources::build_resource_config,
    wave_cache::{AnalysisPersistence, AnalysisTarget, persistence::AnalysisPersistenceConfig},
    waveform::TrackAnalysis,
};

pub(crate) fn chunk_seconds() -> NonZeroU32 {
    NonZeroU32::new(16).expect("fixture chunk duration is non-zero")
}

pub(crate) fn test_pools() -> Pools {
    pools::build().expect("valid app pool policy")
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

pub(crate) fn one_bucket_wave() -> Waveform {
    // version 1 + one bucket of three 0.5 band heights (0.5 = 0x3F000000).
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

/// A settled snapshot covering `[0, covered)` of a 1000-frame track.
pub(crate) fn snapshot(
    token: AnalysisToken,
    revision: u64,
    covered: u64,
    fingerprint: AnalysisFingerprint,
    beat: Option<BeatSnapshot>,
) -> TrackAnalysis {
    let mut coverage = Coverage::default();
    coverage.insert(FrameRange::new(0, covered));
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

pub(crate) fn analysis() -> TrackAnalysis {
    snapshot("test-track".into(), 1, 1_000, fingerprint(), None)
}

pub(crate) fn revision_of(revision: u64) -> TrackAnalysis {
    snapshot("test-track".into(), revision, 1_000, fingerprint(), None)
}

pub(crate) fn revision_held(rx: &watch::Receiver<Option<AnalysisProgress>>) -> Option<u64> {
    rx.borrow().as_ref().map(|p| p.analysis().revision())
}

pub(crate) fn queue() -> (AppHost, AppQueueControl) {
    let worker = AppWorker::new(PlayWorkerConfig::builder(test_pools()).build());
    let mut host = AppHost::new(HostConfig::builder().build()).expect("test host");
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

pub(crate) fn track(queue: &AppQueueControl, id: u64, url: &str) -> (TrackId, AppTrackSource) {
    let track_id = TrackId::from(id);
    queue
        .append_with_id(track_id, url.to_owned())
        .expect("append test track");
    let source = queue.track_source(track_id).expect("track has a source");
    (track_id, source)
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

/// A PCM WAV of `seconds` of a 440 Hz tone at the fixture axis, as a
/// `file://` URL the app opens like any library track.
pub(crate) fn wav_track(directory: &Path, seconds: u32) -> String {
    let path = directory.join("track.wav");
    let rate = axis().get();
    let frames = rate * seconds;
    let data_len = frames * 4;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    let step = std::f64::consts::TAU * 440.0 / f64::from(rate);
    for frame in 0..frames {
        let sample: i16 = ((f64::from(frame) * step).sin() * 16_000.0).as_();
        bytes.extend_from_slice(&sample.to_le_bytes());
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(&path, bytes).expect("fixture track is written");
    format!("file://{}", path.display())
}

/// The entry the owner would create for `source` under `config`.
pub(crate) fn entry(
    config: &AppConfig,
    queue: AppQueueControl,
    track_id: TrackId,
    source: AppTrackSource,
) -> Entry {
    let config = match source {
        AppTrackSource::Config(cfg) => *cfg,
        AppTrackSource::Uri(url) => {
            build_resource_config(&url, config).expect("source yields a resource")
        }
        _ => panic!("fixture source has no resource"),
    };
    let target = AnalysisTarget::for_config(&config).expect("source has an analysis target");
    Entry::new(target, config, queue, track_id)
}
