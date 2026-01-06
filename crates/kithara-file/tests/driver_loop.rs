#![forbid(unsafe_code)]

use std::time::Duration;

use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use futures::StreamExt;
use kithara_file::{FileSource, FileSourceOptions};
use rstest::{fixture, rstest};
use tokio::net::TcpListener;

// NOTE: These integration tests were written for the legacy `kithara-cache` API
// (CacheOptions/max_bytes/root_dir + CachePath + put_atomic).
//
// The project has moved to the resource-based `kithara-assets` + `kithara-storage` architecture.
// This file is kept compiling, but tests are ignored and will be rewritten against the new API.

#[fixture]
fn default_opts() -> FileSourceOptions {
    FileSourceOptions::default()
}

#[fixture]
async fn test_server() -> TestServer {
    TestServer::new().await
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

    fn url(&self, path: &str) -> url::Url {
        format!("{}{}", self.base_url, path).parse().unwrap()
    }
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[ignore = "outdated: will be rewritten for kithara-assets + resource-based API"]
async fn file_stream_downloads_all_bytes_and_closes(
    #[future] test_server: TestServer,
    default_opts: FileSourceOptions,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_opts, None).await.unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    // Stream should have closed (EOS reached)
    assert_eq!(
        received_data,
        b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345"
    );
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[ignore = "outdated: will be rewritten for kithara-assets + resource-based API"]
async fn file_stream_downloads_chunked_content_and_closes(
    #[future] test_server: TestServer,
    default_opts: FileSourceOptions,
) {
    let server = test_server.await;
    let url = server.url("/chunked.mp3");

    let session = FileSource::open(url, default_opts, None).await.unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    // Stream should have closed (EOS reached)
    assert_eq!(received_data, b"Chunk1-Chunk2-Chunk3");
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[ignore = "outdated: will be rewritten for kithara-assets + resource-based API"]
async fn file_receiver_drop_cancels_driver(
    #[future] test_server: TestServer,
    default_opts: FileSourceOptions,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_opts, None).await.unwrap();

    // Create stream and read one chunk
    let mut stream = session.stream().await;
    let first_chunk = stream
        .next()
        .await
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
#[tokio::test]
#[timeout(Duration::from_secs(5))]
#[ignore = "outdated: relied on removed legacy cache API; will be rewritten for kithara-assets + kithara-storage (StreamingResource/AtomicResource)"]
async fn file_offline_replays_from_cache(_default_opts: FileSourceOptions) {
    // Legacy test body intentionally removed. The new offline replay contract is:
    // - resources addressed as <cache_root>/<asset_root>/<rel_path>
    // - small objects via AtomicResource (whole-object read/write, atomic replace)
    // - large objects via StreamingResource (write_at/read_at + wait_range)
    //
    // This test will be rewritten once kithara-file is wired to kithara-assets.
    unimplemented!("rewrite for kithara-assets + kithara-storage offline semantics");
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
#[ignore = "outdated: relied on removed cache API; will be rewritten for kithara-assets + resource-based API"]
async fn file_offline_miss_is_fatal(_default_opts: FileSourceOptions) {
    // Legacy test body intentionally removed. This scenario will be re-specified
    // for kithara-assets + kithara-storage once offline rules are implemented there.
    unimplemented!("rewrite for kithara-assets + kithara-storage (offline rules on resources)");
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[ignore = "outdated: seek contract is being redesigned around StreamingResource + kithara-io Read+Seek; will be rewritten"]
async fn seek_roundtrip_correctness(
    #[future] test_server: TestServer,
    default_opts: FileSourceOptions,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_opts, None).await.unwrap();

    // Start streaming first
    let mut stream = session.stream().await;

    // Read first chunk to ensure driver is running
    let first_chunk = stream
        .next()
        .await
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Try to seek using session's seek_bytes method
    let seek_result = session.seek_bytes(0);

    // Currently seek may return DriverStopped; this test documents the current behavior
    match seek_result {
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::SeekNotSupported)) => {
            // Expected when seek is not implemented
        }
        Err(kithara_file::FileError::DriverStopped) => {
            // Driver might have stopped or command channel closed
        }
        other => panic!(
            "Expected SeekNotSupported or DriverStopped error, got: {:?}",
            other
        ),
    }
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[ignore = "outdated: seek contract is being redesigned around StreamingResource + kithara-io Read+Seek; will be rewritten"]
async fn seek_variants_not_supported(
    #[future] test_server: TestServer,
    default_opts: FileSourceOptions,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_opts, None).await.unwrap();

    // Start streaming first
    let mut stream = session.stream().await;

    // Read first chunk to ensure driver is running
    let first_chunk = stream
        .next()
        .await
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Try to seek using session's seek_bytes method
    let seek_result = session.seek_bytes(10);

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
        other => panic!(
            "Expected SeekNotSupported or DriverStopped error, got: {:?}",
            other
        ),
    }
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[ignore = "outdated: cancellation contract is being redesigned around CancellationToken + wait_range; will be rewritten"]
async fn cancel_behavior_drop_driven(
    #[future] test_server: TestServer,
    default_opts: FileSourceOptions,
) {
    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_opts, None).await.unwrap();

    // Create stream and read some bytes
    let mut stream = session.stream().await;

    // Read first chunk
    let first_chunk = stream
        .next()
        .await
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
    let new_first_chunk = new_stream
        .next()
        .await
        .expect("Should have first chunk on new stream")
        .expect("First chunk should be ok");

    // Should get the same data as before
    assert_eq!(first_chunk, new_first_chunk);
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[ignore = "outdated: seek contract is being redesigned around StreamingResource + kithara-io Read+Seek; will be rewritten"]
async fn seek_contract_invalid_position(
    #[future] test_server: TestServer,
    default_opts: FileSourceOptions,
) {
    // This test documents the expected behavior for invalid seek positions.
    // Currently, seek is not implemented, so we test the error paths.

    let server = test_server.await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, default_opts, None).await.unwrap();

    // Start streaming first
    let mut stream = session.stream().await;

    // Read first chunk to ensure driver is running
    let first_chunk = stream
        .next()
        .await
        .expect("Should have first chunk")
        .expect("First chunk should be ok");

    assert!(!first_chunk.is_empty());

    // Try to seek to a very large position (beyond known size)
    // Currently this will fail with SeekNotSupported or DriverStopped
    // Once implemented, it should fail with InvalidSeekPosition if size is known
    let seek_result = session.seek_bytes(1_000_000_000); // 1GB position

    match seek_result {
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::SeekNotSupported)) => {
            // Expected when seek is not implemented
        }
        Err(kithara_file::FileError::DriverStopped) => {
            // Driver might have stopped or command channel closed
        }
        // TODO: Once seek is implemented, we should also accept:
        // Err(kithara_file::FileError::Driver(kithara_file::DriverError::Options(
        //     kithara_file::OptionsError::InvalidSeekPosition(_)
        // )))
        other => panic!(
            "Expected SeekNotSupported or DriverStopped error, got: {:?}",
            other
        ),
    }
}
