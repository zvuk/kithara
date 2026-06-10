#![cfg(not(target_arch = "wasm32"))]

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara::{
    assets::StoreOptions,
    file::{File, FileConfig, FileSrc},
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancellationToken, thread,
    time::{Duration, sleep, timeout},
    tokio::task::spawn_blocking,
};

struct Consts;
impl Consts {
    const TOTAL_SIZE: usize = 1_024_000;
    const STREAM_CLOSES_AT: usize = 512_000;
}

fn clean_temp_dir() -> TestTempDir {
    let dir = TestTempDir::new();
    let entries: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(entries.is_empty(), "Temp directory should be empty");
    dir
}

/// Test: early stream close + seek beyond downloaded data.
///
/// After sequential stream closes at 512KB, seek to 700KB
/// should trigger on-demand Range request and succeed.
// flash(false): the deadlock guard wraps a raw-tokio spawn_blocking thread invisible
// to the flash engine; a virtual 5s timeout would fire before the real seek completes.
#[kithara::test(
    flash(false),
    tokio,
    tracing("kithara_file=debug,kithara_stream::writer=debug,kithara_storage=debug")
)]
async fn file_stream_closes_early_seek_still_works() {
    let clean_temp_dir = clean_temp_dir();
    let cancel_token = CancellationToken::default();

    let file_data: Vec<u8> = (0..Consts::TOTAL_SIZE).map(|i| (i % 256) as u8).collect();
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(file_data),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::EarlyClose {
            after_bytes: Consts::STREAM_CLOSES_AT,
        },
    });
    let url = handle.url();

    let dl = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::builder()
                .inactivity_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(1))
                .build(),
            CancellationToken::default(),
        ))
        .cancel(cancel_token.clone())
        .build(),
    );

    let config = FileConfig::for_src(FileSrc::Remote(url))
        .store(StoreOptions::new(clean_temp_dir.path()))
        .cancel(cancel_token)
        .look_ahead_bytes(256_000)
        .downloader(dl)
        .build();

    let mut stream = Stream::<File>::new(config).await.unwrap();

    let blocking_task = spawn_blocking(move || {
        let mut buf = [0u8; 10_000];
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Should read initial data");
        tracing::info!("Read {} bytes from initial stream", n);

        let seek_offset = 700_000u64;
        tracing::info!(
            "Seeking to {}KB (beyond {}KB stream)",
            seek_offset / 1024,
            Consts::STREAM_CLOSES_AT / 1024
        );

        match stream.seek(SeekFrom::Start(seek_offset)) {
            Ok(pos) => {
                assert_eq!(pos, seek_offset);
                tracing::info!("Seek succeeded to position {}", pos);

                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "Should read data after seek");

                let expected = (seek_offset % 256) as u8;
                assert_eq!(
                    buf[0], expected,
                    "Data should match: expected {}, got {}",
                    expected, buf[0]
                );

                tracing::info!("Read {} bytes after seek, data verified", n);
                Ok(())
            }
            Err(e) => {
                tracing::error!("Seek failed: {}", e);
                Err(format!("Seek failed: {}", e))
            }
        }
    });

    let result = match timeout(Duration::from_secs(5), blocking_task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => panic!("Blocking task panicked: {:?}", e),
        Err(_) => panic!(
            "DEADLOCK: seek hung waiting for data beyond {}KB. On-demand mode not working.",
            Consts::STREAM_CLOSES_AT / 1024
        ),
    };

    result.unwrap();
}

/// Test: partial download cached on disk → reopen same URL → seek still works.
///
/// Phase 1: download 512KB of 1MB, drop stream.
/// Phase 2: reopen same URL with same cache dir, seek to 700KB → on-demand Range.
#[kithara::test(
    flash(false),
    tokio,
    tracing("kithara_file=debug,kithara_stream::writer=debug,kithara_storage=debug")
)]
async fn partial_cache_resume_works() {
    let cache_dir = clean_temp_dir();

    let file_data: Vec<u8> = (0..Consts::TOTAL_SIZE).map(|i| (i % 256) as u8).collect();
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(file_data),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::EarlyClose {
            after_bytes: Consts::STREAM_CLOSES_AT,
        },
    });
    let url = handle.url();

    let cancel1 = CancellationToken::default();
    let config1 = FileConfig::for_src(FileSrc::Remote(url.clone()))
        .store(StoreOptions::new(cache_dir.path()))
        .cancel(cancel1.clone())
        .look_ahead_bytes(256_000)
        .build();

    let stream1 = Stream::<File>::new(config1).await.unwrap();

    let phase1 = spawn_blocking(move || {
        let mut stream1 = stream1;
        let mut buf = [0u8; 10_000];
        let n = stream1.read(&mut buf).unwrap();
        assert!(n > 0, "Phase 1: should read initial data");
        tracing::info!("Phase 1: read {} bytes", n);

        thread::sleep(Duration::from_millis(500));
    });

    timeout(Duration::from_secs(3), phase1)
        .await
        .expect("Phase 1 timed out")
        .expect("Phase 1 panicked");

    cancel1.cancel();
    sleep(Duration::from_millis(200)).await;
    tracing::info!("Phase 1 complete, stream dropped");

    let cancel2 = CancellationToken::default();
    let config2 = FileConfig::for_src(FileSrc::Remote(url))
        .store(StoreOptions::new(cache_dir.path()))
        .cancel(cancel2.clone())
        .look_ahead_bytes(256_000)
        .build();

    let stream2 = Stream::<File>::new(config2).await.unwrap();

    let phase2 = spawn_blocking(move || {
        let mut stream2 = stream2;
        let seek_offset = 700_000u64;
        tracing::info!(
            "Phase 2: seeking to {}KB (beyond {}KB partial cache)",
            seek_offset / 1024,
            Consts::STREAM_CLOSES_AT / 1024
        );

        let pos = stream2.seek(SeekFrom::Start(seek_offset)).unwrap();
        assert_eq!(pos, seek_offset);

        let mut buf = [0u8; 10_000];
        let n = stream2.read(&mut buf).unwrap();
        assert!(
            n > 0,
            "Phase 2: should read data after seek on resumed partial"
        );

        let expected = (seek_offset % 256) as u8;
        assert_eq!(
            buf[0], expected,
            "Data mismatch at {}: expected {}, got {}",
            seek_offset, expected, buf[0]
        );
        tracing::info!("Phase 2: read {} bytes at 700KB, data verified", n);
    });

    let result = match timeout(Duration::from_secs(5), phase2).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("Phase 2 panicked: {:?}", e)),
        Err(_) => Err("DEADLOCK: resume seek hung. Partial cache resume not working.".to_string()),
    };

    cancel2.cancel();
    result.unwrap();
}
