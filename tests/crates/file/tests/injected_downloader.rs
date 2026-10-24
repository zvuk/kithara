#![cfg(not(target_arch = "wasm32"))]

use std::io::Read;

use kithara::{
    assets::{AssetStore, StorageBackend},
    download::{Downloader, DownloaderConfig, DownloaderEvent},
    events::EventBus,
    file::{File, FileConfig, FileEvent, FileSrc},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc, time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper,
    bufpool_ext::{TestPools, pools},
    hls_fixture::create_test_downloader,
};
use kithara_test_utils::{TestTempDir, temp_dir};

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(2)))]
async fn an_injected_downloader_carries_the_remote_file_requests() {
    let bytes: Vec<_> = (0..4096)
        .map(|i| u8::try_from(i % 251).expect("fixture byte fits"))
        .collect();
    let server = TestServerHelper::new().await;
    let fixture = server.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(bytes.clone()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Throttle {
            chunk: 1024,
            delay_ms: 10,
        },
    });
    let pools = pools();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            CancelToken::never(),
        ))
        .runtime(kithara::platform::tokio::runtime::Handle::current())
        .soft_timeout(Duration::ZERO)
        .build(),
    );
    let bus = EventBus::new(128);
    let mut events = bus.subscribe::<DownloaderEvent>();
    let config = FileConfig::for_src(FileSrc::Remote(fixture.url()))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .downloader(downloader)
        .events(bus)
        .build();
    let mut stream = Stream::<File<TestPools>>::new(config)
        .await
        .expect("remote file opens");
    let actual = spawn_blocking(move || {
        let mut actual = Vec::new();
        stream.read_to_end(&mut actual).expect("read complete file");
        actual
    })
    .await
    .expect("reader task");
    assert_eq!(actual, bytes);
    let mut reported = false;
    while let Ok(envelope) = events.try_recv() {
        reported |= matches!(envelope.event, DownloaderEvent::LoadSlow { .. });
    }
    assert!(
        reported,
        "the injected Downloader must carry the file requests"
    );
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(2)))]
#[case::uncached(false)]
#[case::cached(true)]
async fn a_remote_file_without_a_downloader_is_refused(
    #[case] cached: bool,
    temp_dir: TestTempDir,
) {
    let bytes = vec![42; 4096];
    let server = TestServerHelper::new().await;
    let fixture = server.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(bytes.clone()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .build();
    if cached {
        let bus = EventBus::new(16);
        let mut completion = bus.subscribe::<FileEvent>();
        let warm = FileConfig::for_src(FileSrc::Remote(fixture.url()))
            .store(store.clone())
            .downloader(create_test_downloader())
            .events(bus)
            .build();
        let mut warm = Stream::<File<TestPools>>::new(warm)
            .await
            .expect("warm remote file");
        let (actual, warm) = spawn_blocking(move || {
            let mut actual = Vec::new();
            warm.read_to_end(&mut actual).expect("warm the whole cache");
            (actual, warm)
        })
        .await
        .expect("cache reader");
        assert_eq!(actual, bytes);
        loop {
            let envelope = completion.recv().await.expect("cache completion event");
            if matches!(envelope.event, FileEvent::CacheComplete { .. }) {
                break;
            }
        }
        drop(warm);
    }
    let config = FileConfig::for_src(FileSrc::Remote(fixture.url()))
        .store(store)
        .build();
    let Err(error) = Stream::<File<TestPools>>::new(config).await else {
        panic!("a remote source opened without a Downloader");
    };
    assert!(
        error.to_string().contains("requires a Downloader"),
        "{error}"
    );
}
