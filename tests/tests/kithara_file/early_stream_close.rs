#![cfg(not(target_arch = "wasm32"))]

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara::{
    assets::StoreOptions,
    events::{DownloaderEvent, Event, EventBus, EventReceiver, FileEvent},
    file::{File, FileConfig, FileSrc},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        flash::real_io,
        time::{self, Duration, Instant},
        tokio::task::spawn_blocking,
    },
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir,
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

/// Wait until the sequential download has reached a terminal state — the
/// real signal that every byte the early-close server is willing to deliver
/// has been committed to disk. The early-close GET ends as either a
/// `RequestCompleted` (server sent its single chunk and closed) or a
/// `RequestFailed` (premature EOF vs the advertised Content-Length); either
/// one means the partial cache is now on disk. `rx` must be subscribed
/// BEFORE the stream starts downloading so the terminal publish is not raced.
async fn wait_for_download_terminal(rx: &mut EventReceiver, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match time::timeout(remaining, rx.recv()).await {
            Ok(Ok(Event::Downloader(DownloaderEvent::RequestCompleted { .. }))) => return true,
            Ok(Ok(Event::Downloader(DownloaderEvent::RequestFailed { .. }))) => return true,
            Ok(Ok(Event::File(FileEvent::Error { .. }))) => return true,
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => return false,
        }
    }
}

/// Test: early stream close + seek beyond downloaded data.
///
/// After the sequential stream closes at 512KB, a seek to 700KB still
/// resolves: the file peer's gap walk resumes the truncated transfer
/// (Range GET from the early-close point) so every byte the seek targets
/// lands in the partial cache. The seek must succeed and read the correct
/// bytes at 700KB — proof the on-demand resume path works.
#[kithara::test(
    tokio,
    tracing("kithara_file=debug,kithara::stream::writer=debug,kithara_storage=debug")
)]
async fn file_stream_closes_early_seek_still_works() {
    let clean_temp_dir = clean_temp_dir();
    let cancel_token = CancelToken::never();

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
                .build(),
            CancelToken::never(),
        ))
        .cancel(cancel_token.clone())
        .build(),
    );

    // Subscribe BEFORE the stream starts downloading so the terminal
    // publish (sequential GET + early-close resume) is not raced.
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();

    let config = FileConfig::for_src(FileSrc::Remote(url))
        .events(bus)
        .store(StoreOptions::new(clean_temp_dir.path()))
        .cancel(cancel_token)
        .look_ahead_bytes(256_000)
        .downloader(dl)
        .build();

    let stream = Stream::<File>::new(config).await.unwrap();

    // Phase 1: prove the initial sequential bytes are readable, then hand
    // the stream back so the seek runs after a settled download state.
    let phase1 = spawn_blocking(move || {
        let mut stream = stream;
        let mut buf = [0u8; 10_000];
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Should read initial data");
        tracing::info!("Read {} bytes from initial stream", n);
        stream
    });

    let stream = time::timeout(Duration::from_secs(5), phase1)
        .await
        .expect("Phase 1 timed out")
        .expect("Phase 1 panicked");

    // Wait on the real state: the sequential GET terminating means the
    // early-close resume has flushed every available byte (here the whole
    // file, resumed past the 512KB close) to the partial cache. Awaiting
    // the event parks on the virtual clock AND resolves on real download
    // state — no timer pacing the seek against an in-flight fetch.
    let flushed = wait_for_download_terminal(&mut rx, Duration::from_secs(5)).await;
    assert!(
        flushed,
        "sequential download did not reach a terminal state; partial cache not flushed",
    );

    // Phase 2: seek beyond the early-close point and read the bytes the
    // resume committed to the partial cache.
    let phase2 = spawn_blocking(move || {
        let mut stream = stream;
        let mut buf = [0u8; 10_000];
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

    let result = match time::timeout(Duration::from_secs(5), phase2).await {
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
    tokio,
    tracing("kithara_file=debug,kithara::stream::writer=debug,kithara_storage=debug")
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

    let cancel1 = CancelToken::never();
    let bus1 = EventBus::new(64);
    let mut rx1 = bus1.subscribe();
    let config1 = FileConfig::for_src(FileSrc::Remote(url.clone()))
        .events(bus1)
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
        stream1
    });

    let stream1 = time::timeout(Duration::from_secs(3), phase1)
        .await
        .expect("Phase 1 timed out")
        .expect("Phase 1 panicked");

    // Keep `stream1` alive and wait on the real state: the sequential
    // download terminating means the look-ahead has flushed every available
    // byte to the partial cache. No timer pacing — phase 2 only resumes once
    // the partial data provably exists on disk.
    let flushed = wait_for_download_terminal(&mut rx1, Duration::from_secs(5)).await;
    assert!(
        flushed,
        "Phase 1: sequential download did not reach a terminal state; partial cache not flushed",
    );

    cancel1.cancel();
    drop(stream1);
    tracing::info!("Phase 1 complete, stream dropped");

    let cancel2 = CancelToken::never();
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

    // The phase-2 seek to 700KB drives a gap-walk Range-resume GET on a REAL
    // socket. Hold a `RealIoScope` across the wait so the virtual clock is paced
    // to real time: the 5 s budget then fires only after the equivalent REAL
    // time, never spuriously ahead of bytes still in transit. The budget stays a
    // genuine stall oracle (a truly stuck resume still exhausts the paced 5 s).
    // Off the `flash` feature the scope is a ZST no-op and the clock is real.
    let result = {
        let _real_io = real_io();
        match time::timeout(Duration::from_secs(5), phase2).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("Phase 2 panicked: {:?}", e)),
            Err(_) => {
                Err("DEADLOCK: resume seek hung. Partial cache resume not working.".to_string())
            }
        }
    };

    cancel2.cancel();
    result.unwrap();
}
