#![forbid(unsafe_code)]

use std::time::Duration;

use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::StoreOptions;
use kithara_file::{FileParams, FileSource};
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio::{net::TcpListener, time::timeout};

// ==================== Test Server Fixtures ====================

#[fixture]
async fn test_server() -> TestServer {
    TestServer::new().await
}

#[fixture]
async fn test_server_with_chunked() -> TestServer {
    TestServer::with_chunked().await
}

struct TestServer {
    base_url: String,
    _server_handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = Router::new()
            .route(
                "/test.mp3",
                get(|| async {
                    Response::builder()
                        .status(200)
                        .body(axum::body::Body::from(Bytes::from_static(
                            b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345",
                        )))
                        .unwrap()
                }),
            )
            .route(
                "/small.bin",
                get(|| async {
                    Response::builder()
                        .status(200)
                        .body(axum::body::Body::from(Bytes::from_static(
                            b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
                        )))
                        .unwrap()
                }),
            )
            .route(
                "/empty.bin",
                get(|| async {
                    Response::builder()
                        .status(200)
                        .body(axum::body::Body::from(Bytes::from_static(b"")))
                        .unwrap()
                }),
            );

        let server_handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give server time to start
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Self {
            base_url: format!("http://127.0.0.1:{}", addr.port()),
            _server_handle: server_handle,
        }
    }

    async fn with_chunked() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = Router::new()
            .route(
                "/chunked.mp3",
                get(|| async {
                    Response::builder()
                        .status(200)
                        .body(axum::body::Body::from_stream(futures::stream::iter(vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"Chunk1-")),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"Chunk2-")),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"Chunk3")),
                        ])))
                        .unwrap()
                }),
            )
            .route(
                "/chunked-large.bin",
                get(|| async {
                    Response::builder()
                        .status(200)
                        .body(axum::body::Body::from_stream(futures::stream::iter(vec![
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"Part1-")),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"Part2-")),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"Part3-")),
                            Ok::<Bytes, std::io::Error>(Bytes::from_static(b"Part4")),
                        ])))
                        .unwrap()
                }),
            );

        let server_handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Self {
            base_url: format!("http://127.0.0.1:{}", addr.port()),
            _server_handle: server_handle,
        }
    }

    fn url(&self, path: &str) -> url::Url {
        format!("{}{}", self.base_url, path).parse().unwrap()
    }
}

// ==================== FileParams Fixtures ====================

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[fixture]
fn default_params(temp_dir: TempDir) -> FileParams {
    FileParams::new(StoreOptions::new(temp_dir.path()))
}

#[fixture]
fn params_with_small_buffer(temp_dir: TempDir) -> FileParams {
    FileParams::new(StoreOptions::new(temp_dir.path())).with_max_buffer_size(16 * 1024)
}

#[fixture]
fn params_with_large_buffer(temp_dir: TempDir) -> FileParams {
    FileParams::new(StoreOptions::new(temp_dir.path())).with_max_buffer_size(4096 * 1024)
}

// ==================== Test Cases ====================

#[rstest]
#[case("/test.mp3", b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345")]
#[case("/small.bin", b"ABCDEFGHIJKLMNOPQRSTUVWXYZ")]
#[case("/empty.bin", b"")]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn file_stream_downloads_all_bytes_and_closes(
    #[future] test_server: TestServer,
    default_params: FileParams,
    #[case] path: &str,
    #[case] expected_data: &[u8],
) {
    let server = test_server.await;
    let url = server.url(path);

    let session = FileSource::open(url, default_params).await.unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    // Stream should have closed (EOS reached)
    assert_eq!(received_data, expected_data);
}

#[rstest]
#[case("/chunked.mp3", b"Chunk1-Chunk2-Chunk3")]
#[case("/chunked-large.bin", b"Part1-Part2-Part3-Part4")]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn file_stream_downloads_chunked_content_and_closes(
    #[future] test_server_with_chunked: TestServer,
    default_params: FileParams,
    #[case] path: &str,
    #[case] expected_data: &[u8],
) {
    let server = test_server_with_chunked.await;
    let url = server.url(path);

    let session = FileSource::open(url, default_params).await.unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    // Stream should have closed (EOS reached)
    assert_eq!(received_data, expected_data);
}

#[rstest]
#[case("/test.mp3")]
#[case("/small.bin")]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn file_receiver_drop_cancels_driver(
    #[future] test_server: TestServer,
    default_params: FileParams,
    #[case] path: &str,
) {
    let server = test_server.await;
    let url = server.url(path);

    let session = FileSource::open(url, default_params).await.unwrap();

    // Create stream and read one chunk
    let mut stream = session.stream().await;
    let first_chunk = timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("Should receive first chunk within timeout")
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Drop the stream (simulating consumer stopping)
    drop(stream);

    // Driver should cancel without hanging
    // We can't directly test driver cancellation, but we can verify
    // that the test doesn't hang or panic
}

#[rstest]
#[case("/test.mp3")]
#[case("/small.bin")]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn multiple_streams_from_same_session(
    #[future] test_server: TestServer,
    default_params: FileParams,
    #[case] path: &str,
) {
    let server = test_server.await;
    let url = server.url(path);

    let session = FileSource::open(url, default_params).await.unwrap();

    // Create first stream and read some data
    let mut stream1 = session.stream().await;
    let chunk1 = stream1
        .next()
        .await
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    // Create second stream from same session
    let mut stream2 = session.stream().await;
    let chunk2 = stream2
        .next()
        .await
        .expect("Should have first chunk on second stream")
        .expect("First chunk should be ok");

    // Both streams should get the same initial data
    assert_eq!(chunk1, chunk2);
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn file_stream_works_with_small_buffer(
    #[future] test_server: TestServer,
    params_with_small_buffer: FileParams,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, params_with_small_buffer)
        .await
        .unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    assert_eq!(
        received_data,
        b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345"
    );
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn file_stream_works_with_large_buffer(
    #[future] test_server: TestServer,
    params_with_large_buffer: FileParams,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, params_with_large_buffer)
        .await
        .unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    assert_eq!(
        received_data,
        b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345"
    );
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn seek_roundtrip_correctness(#[future] test_server: TestServer, default_params: FileParams) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_params).await.unwrap();

    // Start streaming first
    let mut stream = session.stream().await;

    // Read first chunk to ensure driver is running
    let first_chunk = timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("Should receive first chunk within timeout")
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Try to seek using session's seek_bytes method
    let seek_result = session.seek_bytes(0).await;

    // Currently seek may return DriverStopped or ChannelClosed; this test documents the current behavior
    match seek_result {
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::SeekNotSupported)) => {
            // Expected when seek is not implemented
        }
        Err(kithara_file::FileError::DriverStopped) => {
            // Driver might have stopped or command channel closed
        }
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::Stream(
            kithara_stream::StreamError::ChannelClosed,
        ))) => {
            // Channel closed - driver might have terminated
        }
        other => panic!(
            "Expected SeekNotSupported, DriverStopped, or ChannelClosed error, got: {:?}",
            other
        ),
    }
}

#[rstest]
#[case(0)]
#[case(10)]
#[case(100)]
#[case(1000)]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn seek_variants_not_supported(
    #[future] test_server: TestServer,
    default_params: FileParams,
    #[case] seek_pos: u64,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_params).await.unwrap();

    // Start streaming first
    let mut stream = session.stream().await;

    // Read first chunk to ensure driver is running
    let first_chunk = timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("Should receive first chunk within timeout")
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Try to seek using session's seek_bytes method
    let seek_result = session.seek_bytes(seek_pos).await;

    // Should return error (seek behavior is still being redesigned)
    assert!(seek_result.is_err());

    // Check error type
    match seek_result {
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::SeekNotSupported)) => {
            // Expected error
        }
        Err(kithara_file::FileError::DriverStopped) => {
            // Driver might have stopped or command channel closed
        }
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::Stream(
            kithara_stream::StreamError::ChannelClosed,
        ))) => {
            // Channel closed - driver might have terminated
        }
        other => panic!(
            "Expected SeekNotSupported, DriverStopped, or ChannelClosed error, got: {:?}",
            other
        ),
    }
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn cancel_behavior_drop_driven(
    #[future] test_server: TestServer,
    default_params: FileParams,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_params).await.unwrap();

    // Create stream and read some bytes
    let mut stream = session.stream().await;

    // Read first chunk
    let first_chunk = timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("Should receive first chunk within timeout")
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Drop the stream (simulating consumer stopping)
    drop(stream);

    // Driver should cancel without hanging
    // We can't directly test driver cancellation, but we can verify
    // that the test doesn't hang or panic

    // Try to create a new stream from the same session
    // This should work (session should still be valid)
    let mut new_stream = session.stream().await;

    // New stream should start from beginning
    let new_first_chunk = timeout(Duration::from_secs(5), new_stream.next())
        .await
        .expect("Should receive first chunk on new stream within timeout")
        .expect("Should have first chunk on new stream")
        .expect("First chunk should be ok");

    // Should get the same data as before
    assert_eq!(first_chunk, new_first_chunk);
}

#[rstest]
#[case(1_000_000)]
#[case(10_000_000)]
#[case(100_000_000)]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn seek_contract_invalid_position(
    #[future] test_server: TestServer,
    default_params: FileParams,
    #[case] seek_pos: u64,
) {
    // This test documents the expected behavior for invalid seek positions.
    // Currently, seek is not implemented, so we test the error paths.

    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_params).await.unwrap();

    // Start streaming first
    let mut stream = session.stream().await;

    // Read first chunk to ensure driver is running
    let first_chunk = timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("Should receive first chunk within timeout")
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Try to seek to a very large position (beyond known size)
    // Currently this will fail with SeekNotSupported or DriverStopped
    let seek_result = session.seek_bytes(seek_pos).await;

    match seek_result {
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::SeekNotSupported)) => {
            // Expected when seek is not implemented
        }
        Err(kithara_file::FileError::DriverStopped) => {
            // Driver might have stopped or command channel closed
        }
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::Stream(
            kithara_stream::StreamError::ChannelClosed,
        ))) => {
            // Channel closed - driver might have terminated
        }
        other => panic!(
            "Expected SeekNotSupported, DriverStopped, or ChannelClosed error, got: {:?}",
            other
        ),
    }
}

// ==================== Legacy Tests (Ignored) ====================

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
#[ignore = "rewrite for kithara-assets + kithara-storage offline semantics"]
async fn file_offline_replays_from_cache(_default_params: FileParams) {
    // Legacy test body intentionally removed. The new offline replay contract is:
    // - resources addressed as <cache_root>/<asset_root>/<rel_path>
    // - small objects via AtomicResource (whole-object read/write, atomic replace)
    // - large objects via StreamingResource (write_at/read_at + wait_range)
    //
    // This test will be rewritten once kithara-file is wired to kithara-assets.
    unimplemented!("rewrite for kithara-assets + kithara-storage offline semantics");
}
