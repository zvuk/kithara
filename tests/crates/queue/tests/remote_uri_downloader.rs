#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, StorageBackend},
    download::{Downloader, DownloaderConfig},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelScope, time::Duration},
    play::{PlayWorker, PlayWorkerConfig, ResourceConfig, ResourcePrep, ResourceSrc},
    queue::{Queue, QueueConfig, QueueEvent, TrackSource, TrackStatus, Transition},
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    hls_fixture::create_test_downloader,
    offline::{OfflineQueue, QueueTicker, RENDER_PACE},
};
use kithara_test_fixtures::SignalAsset;

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(2)))]
#[case::absent(false)]
#[case::present(true)]
async fn a_remote_uri_track_takes_the_queue_downloader(#[case] present: bool) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE1K_48K_1S);
    let status = load_track(
        TrackSource::Uri(url.to_string()),
        present.then(create_test_downloader),
    )
    .await;
    if present {
        assert!(matches!(status, TrackStatus::Loaded), "{status:?}");
    } else {
        let TrackStatus::Failed(error) = status else {
            panic!("a remote URI loaded without a Downloader");
        };
        assert!(error.contains("requires a Downloader"), "{error}");
    }
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(2)))]
async fn a_caller_built_config_keeps_its_downloader() {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE1K_48K_1S);
    let scope = CancelScope::new(None);
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools(),
            scope.token().child(),
        ))
        .runtime(kithara::platform::tokio::runtime::Handle::current())
        .cancel(scope.token())
        .build(),
    );
    scope.cancel();
    let source = ResourceConfig::for_src(ResourceSrc::Url(url))
        .store(AssetStore::builder(pools()).build())
        .downloader(create_test_downloader())
        .build();
    let status = load_track(TrackSource::Config(Box::new(source)), Some(downloader)).await;
    assert!(matches!(status, TrackStatus::Loaded), "{status:?}");
}

async fn load_track(source: TrackSource<TestPools>, downloader: Option<Downloader>) -> TrackStatus {
    let region = pools();
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(
            PlayWorkerConfig::builder(region.clone()).build(),
        ))
        .build();
    let queue = Queue::new(
        QueueConfig::builder()
            .maybe_downloader(downloader)
            .prep(prep)
            .store(
                AssetStore::builder(region.clone())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build(),
    );
    let queue = OfflineQueue::paced(HostConfig::offline(region).build(), queue, RENDER_PACE)
        .await
        .expect("offline queue");
    let ticker = QueueTicker::spawn(queue.control(), Duration::from_millis(10));
    let mut events = queue.subscribe::<QueueEvent>();
    let id = queue
        .run(move |q| q.append(source))
        .await
        .expect("append remote URI");
    let _ = queue.run(move |q| q.select(id, Transition::None)).await;
    let status = loop {
        let envelope = events.recv().await.expect("queue stays open");
        if let QueueEvent::TrackStatusChanged {
            id: changed,
            status,
        } = envelope.event
            && changed == id
            && matches!(status, TrackStatus::Failed(_) | TrackStatus::Loaded)
        {
            break status;
        }
    };
    drop(ticker);
    status
}
