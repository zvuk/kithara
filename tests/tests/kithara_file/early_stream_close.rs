//! Test: File with early stream close + seek via on-demand Range request.
//!
//! Scenario:
//! 1. HTTP server advertises Content-Length via HEAD but only sends partial data
//! 2. Sequential stream closes at 512KB of 1MB file
//! 3. Seek to 700KB triggers on-demand Range request
//! 4. Data is fetched and read correctly

use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use kithara_assets::StoreOptions;
use kithara_file::{File, FileConfig, FileSrc};
use kithara_stream::Stream;
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::oneshot};
use tokio_util::sync::CancellationToken;

const TOTAL_SIZE: usize = 1_024_000;
const STREAM_CLOSES_AT: usize = 512_000;

fn clean_temp_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(entries.is_empty(), "Temp directory should be empty");
    dir
}

#[derive(Clone)]
struct ServerState {
    file_data: Vec<u8>,
    call_count: Arc<AtomicUsize>,
}

async fn handle_request(
    State(state): State<ServerState>,
    headers: HeaderMap,
    req: Request,
) -> Response {
    let call_num = state.call_count.fetch_add(1, Ordering::SeqCst);
    let method = req.method().clone();

    // HEAD request — return correct Content-Length
    if method == axum::http::Method::HEAD {
        tracing::info!("Server: HEAD request - Content-Length: {}", TOTAL_SIZE);
        return (
            StatusCode::OK,
            [
                (header::CONTENT_LENGTH, TOTAL_SIZE.to_string()),
                (header::CONTENT_TYPE, "audio/mpeg".to_string()),
            ],
        )
            .into_response();
    }

    // Parse Range header
    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    tracing::info!(
        "Server: {} #{}, Range: {:?}",
        method,
        call_num,
        range_header
    );

    if let Some(range_str) = range_header {
        // Range request — serve requested range fully
        if let Some(range_part) = range_str.strip_prefix("bytes=") {
            let parts: Vec<&str> = range_part.split('-').collect();
            let start = parts[0].parse::<usize>().unwrap_or(0);
            let end = parts
                .get(1)
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(TOTAL_SIZE - 1);

            let end = (end + 1).min(TOTAL_SIZE);
            tracing::info!("Server: serving range [{}, {})", start, end);

            let chunk = Bytes::from(state.file_data[start..end].to_vec());
            let content_range = format!("bytes {}-{}/{}", start, end - 1, TOTAL_SIZE);

            return (
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_LENGTH, (end - start).to_string()),
                    (header::CONTENT_RANGE, content_range),
                    (header::CONTENT_TYPE, "audio/mpeg".to_string()),
                ],
                chunk,
            )
                .into_response();
        }
    }

    // Sequential request — only send partial data (simulates early close)
    tracing::warn!(
        "Server: sequential request - sends {}KB of {}KB",
        STREAM_CLOSES_AT / 1024,
        TOTAL_SIZE / 1024
    );

    let chunk = Bytes::from(state.file_data[0..STREAM_CLOSES_AT].to_vec());

    (
        StatusCode::OK,
        [
            (header::CONTENT_LENGTH, STREAM_CLOSES_AT.to_string()),
            (header::CONTENT_TYPE, "audio/mpeg".to_string()),
        ],
        Body::from(chunk),
    )
        .into_response()
}

/// Shared server setup for tests.
async fn setup_server(file_data: Vec<u8>) -> (url::Url, Arc<AtomicUsize>, oneshot::Sender<()>) {
    let state = ServerState {
        file_data,
        call_count: Arc::new(AtomicUsize::new(0)),
    };

    let call_count = state.call_count.clone();

    let app = Router::new()
        .route("/test.mp3", get(handle_request).head(handle_request))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url: url::Url = format!("http://{}/test.mp3", addr).parse().unwrap();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    (url, call_count, shutdown_tx)
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::default()
                .add_directive("kithara_file=debug".parse().unwrap())
                .add_directive("kithara_stream::writer=debug".parse().unwrap())
                .add_directive("kithara_storage=debug".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();
}

/// Test: early stream close + seek beyond downloaded data.
///
/// After sequential stream closes at 512KB, seek to 700KB
/// should trigger on-demand Range request and succeed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_stream_closes_early_seek_still_works() {
    let clean_temp_dir = clean_temp_dir();
    let cancel_token = CancellationToken::new();
    init_tracing();

    let file_data: Vec<u8> = (0..TOTAL_SIZE).map(|i| (i % 256) as u8).collect();
    let (url, _call_count, shutdown_tx) = setup_server(file_data).await;

    let config = FileConfig::new(FileSrc::Remote(url))
        .with_store(StoreOptions::new(clean_temp_dir.path()))
        .with_cancel(cancel_token)
        .with_look_ahead_bytes(256_000);

    let mut stream = Stream::<File>::new(config).await.unwrap();

    let blocking_task = tokio::task::spawn_blocking(move || {
        // Read first chunk from initial partial stream
        let mut buf = [0u8; 10_000];
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Should read initial data");
        tracing::info!("Read {} bytes from initial stream", n);

        // Seek beyond initial 512KB stream → triggers on-demand Range request
        let seek_offset = 700_000u64;
        tracing::info!(
            "Seeking to {}KB (beyond {}KB stream)",
            seek_offset / 1024,
            STREAM_CLOSES_AT / 1024
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

    let result = match tokio::time::timeout(Duration::from_secs(5), blocking_task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => panic!("Blocking task panicked: {:?}", e),
        Err(_) => panic!(
            "DEADLOCK: seek hung waiting for data beyond {}KB. On-demand mode not working.",
            STREAM_CLOSES_AT / 1024
        ),
    };

    let _ = shutdown_tx.send(());
    result.unwrap();
}

/// Test: partial download cached on disk → reopen same URL → seek still works.
///
/// Phase 1: download 512KB of 1MB, drop stream.
/// Phase 2: reopen same URL with same cache dir, seek to 700KB → on-demand Range.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_cache_resume_works() {
    let cache_dir = clean_temp_dir();
    init_tracing();

    let file_data: Vec<u8> = (0..TOTAL_SIZE).map(|i| (i % 256) as u8).collect();
    let (url, _call_count, shutdown_tx) = setup_server(file_data).await;

    // ── Phase 1: partial download, then drop ──
    let cancel1 = CancellationToken::new();
    let config1 = FileConfig::new(FileSrc::Remote(url.clone()))
        .with_store(StoreOptions::new(cache_dir.path()))
        .with_cancel(cancel1.clone())
        .with_look_ahead_bytes(256_000);

    let stream1 = Stream::<File>::new(config1).await.unwrap();

    let phase1 = tokio::task::spawn_blocking(move || {
        let mut stream1 = stream1;
        let mut buf = [0u8; 10_000];
        let n = stream1.read(&mut buf).unwrap();
        assert!(n > 0, "Phase 1: should read initial data");
        tracing::info!("Phase 1: read {} bytes", n);

        // Give sequential download time to finish (server sends 512KB quickly)
        std::thread::sleep(Duration::from_millis(500));
        // stream1 drops here
    });

    tokio::time::timeout(Duration::from_secs(3), phase1)
        .await
        .expect("Phase 1 timed out")
        .expect("Phase 1 panicked");

    cancel1.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    tracing::info!("Phase 1 complete, stream dropped");

    // ── Phase 2: reopen same URL + cache dir, seek beyond partial ──
    let cancel2 = CancellationToken::new();
    let config2 = FileConfig::new(FileSrc::Remote(url))
        .with_store(StoreOptions::new(cache_dir.path()))
        .with_cancel(cancel2.clone())
        .with_look_ahead_bytes(256_000);

    let stream2 = Stream::<File>::new(config2).await.unwrap();

    let phase2 = tokio::task::spawn_blocking(move || {
        let mut stream2 = stream2;
        let seek_offset = 700_000u64;
        tracing::info!(
            "Phase 2: seeking to {}KB (beyond {}KB partial cache)",
            seek_offset / 1024,
            STREAM_CLOSES_AT / 1024
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

    let result = match tokio::time::timeout(Duration::from_secs(5), phase2).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("Phase 2 panicked: {:?}", e)),
        Err(_) => Err("DEADLOCK: resume seek hung. Partial cache resume not working.".to_string()),
    };

    let _ = shutdown_tx.send(());
    cancel2.cancel();
    result.unwrap();
}
