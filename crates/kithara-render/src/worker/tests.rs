use kithara_assets::{AssetStore, StorageBackend};
use kithara_audio::{AudioConfig, NoResamplerBackend};
use kithara_file::{File, FileConfig, FileSrc};
use kithara_test_utils::{cancel_token, kithara};

use super::{TrackConfig, TrackConfigPatch};
use crate::test_pools::{TestPools, pools};

fn file_config() -> FileConfig<TestPools> {
    let pools = pools();
    FileConfig::for_src(FileSrc::Local(
        std::env::temp_dir().join("kithara-audio-config.wav"),
    ))
    .store(
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .build(),
    )
    .build()
}

#[kithara::test(tokio)]
async fn cold_source_preparation_runs_outside_the_requesting_thread() {
    use kithara_platform::{thread, time::Duration};
    use kithara_test_fixtures::signal::{Wave, wav};
    use kithara_test_utils::{TestTempDir, test::usdt};

    let directory = TestTempDir::new();
    let path = directory.path().join("preparation.wav");
    std::fs::write(&path, wav(44_100, 2, 44_100, Wave::Silence)).expect("write source");
    let pools = pools();
    let file = FileConfig::for_src(FileSrc::Local(path))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let audio = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file).build();
    let config = TrackConfig::for_audio(audio).build();
    let start = crate::LaneStart {
        speed: kithara_sync::SpeedCurve::Constant(1.0),
        keylock: false,
        backend: config.warp().backend(),
    };
    let worker = super::PlayWorker::new(super::PlayWorkerConfig::builder(pools).build());
    let (_sender, inbox) = worker.lane_channel();
    let caller = thread::current().id();
    let trace = usdt::scope();
    let (_receiver, _lane, _latency) = worker
        .load(config, Duration::ZERO, start, inbox)
        .await
        .expect("prepare source");
    let opened = trace.events_of("source_opened");
    drop(trace);
    assert_eq!(opened.len(), 1, "one real source-open operation");
    assert_ne!(
        opened[0].thread, caller,
        "cold source work must not occupy its requesting owner"
    );
}

#[kithara::test]
fn audio_config_keeps_the_native_ring_and_preload_defaults() {
    let audio =
        AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file_config()).build();
    let config = TrackConfig::for_audio(audio).build();

    assert_eq!(config.audio_buffer_chunks().get(), 10);
    assert_eq!(config.preload_chunks().get(), 3);
}

#[kithara::test]
fn the_document_names_both_live_keys() {
    let audio =
        AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file_config()).build();
    let config = TrackConfig::for_audio(audio)
        .preload_chunks(std::num::NonZeroUsize::new(8).expect("preload"))
        .audio_buffer_chunks(std::num::NonZeroUsize::new(20).expect("capacity"))
        .build();
    assert_eq!(
        config.preload_chunks(),
        std::num::NonZeroUsize::new(8).expect("preload")
    );
    assert_eq!(config.audio_buffer_chunks().get(), 20);
}

#[kithara::test]
fn an_absent_key_stays_unset_rather_than_defaulting() {
    let audio =
        AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file_config()).build();
    let mut config = TrackConfig::for_audio(audio)
        .audio_buffer_chunks(crate::consts::CAPACITY)
        .build();
    let patch: TrackConfigPatch =
        serde_yaml_ng::from_str("preload_chunks: 8\n").expect("the document types");
    assert!(patch.audio_buffer_chunks.is_none());
    config.apply(patch);
    assert_eq!(
        config.preload_chunks(),
        std::num::NonZeroUsize::new(8).expect("preload")
    );
    assert_eq!(
        config.audio_buffer_chunks(),
        crate::consts::CAPACITY,
        "an unnamed value leaves the owner's capacity unchanged"
    );
}

#[kithara::test(tokio)]
async fn domain_cancel_interrupts_cold_open_on_a_shared_worker(
    cancel_token: kithara_platform::CancelToken,
) {
    use kithara_platform::{
        time::{Duration, timeout},
        tokio::sync::oneshot,
    };
    use kithara_stream::{SourceError, StreamType};
    use kithara_worker::{Worker, WorkerConfig};

    struct PendingFile;
    impl StreamType for PendingFile {
        type Config = oneshot::Sender<()>;
        type Events = kithara_events::EventBus;
        type Source = <File<TestPools> as StreamType>::Source;
        async fn create(started: Self::Config) -> Result<Self::Source, SourceError> {
            let _ = started.send(());
            std::future::pending().await
        }
    }

    let shared = Worker::new(WorkerConfig::new());
    let worker = super::PlayWorker::new(
        super::PlayWorkerConfig::builder(pools())
            .worker(shared.clone())
            .cancel(cancel_token.clone())
            .build(),
    );
    let (started, received) = oneshot::channel();
    let audio = AudioConfig::<PendingFile, NoResamplerBackend>::for_stream(started).build();
    let config = TrackConfig::for_audio(audio).build();
    let start = crate::LaneStart {
        speed: kithara_sync::SpeedCurve::Constant(1.0),
        keylock: false,
        backend: config.warp().backend(),
    };
    let (_sender, inbox) = worker.lane_channel();
    let load = worker.load(config, Duration::ZERO, start, inbox);
    let (result, ()) =
        kithara_platform::tokio::join!(timeout(Duration::from_secs(1), load), async {
            received.await.expect("cold source starts");
            cancel_token.cancel();
        },);
    assert!(matches!(
        result.expect("domain cancellation releases the load"),
        Err(super::LoadRefusal::Cancelled)
    ));
    assert!(
        !shared.is_cancelled(),
        "another owner of the base worker remains live"
    );
}

#[path = "cold_open_tests.rs"]
mod cold_open;
