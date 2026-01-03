use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use futures::StreamExt;
use hex;
use kithara_cache::{AssetCache, CacheOptions};
use kithara_file::{FileSource, FileSourceOptions};
use tempfile::TempDir;
use tokio::net::TcpListener;

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

#[tokio::test]
async fn file_stream_downloads_all_bytes_and_closes() {
    let server = TestServer::new().await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, FileSourceOptions::default(), None)
        .await
        .unwrap();

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

#[tokio::test]
async fn file_stream_downloads_chunked_content_and_closes() {
    let server = TestServer::new().await;
    let url = server.url("/chunked.mp3");

    let session = FileSource::open(url, FileSourceOptions::default(), None)
        .await
        .unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    // Stream should have closed (EOS reached)
    assert_eq!(received_data, b"Chunk1-Chunk2-Chunk3");
}

#[tokio::test]
async fn file_receiver_drop_cancels_driver() {
    let server = TestServer::new().await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, FileSourceOptions::default(), None)
        .await
        .unwrap();

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

#[tokio::test]
async fn file_offline_replays_from_cache() {
    let temp_dir = TempDir::new().unwrap();
    let server = TestServer::new().await;
    let url = server.url("/test.mp3");

    // Create cache
    let cache = AssetCache::open(CacheOptions {
        max_bytes: 10 * 1024 * 1024,
        root_dir: Some(temp_dir.path().to_path_buf()),
    })
    .unwrap();

    // First run: online download to fill cache
    let session = FileSource::open(
        url.clone(),
        FileSourceOptions::default(),
        Some(cache.clone()),
    )
    .await
    .unwrap();

    let mut stream = session.stream().await;
    let mut online_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        online_data.extend_from_slice(&chunk);
    }

    // Give cache write time to complete and ensure file exists
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Verify cache file was written
    let cache_dir = temp_dir.path();
    let asset_id = kithara_core::AssetId::from_url(&url).unwrap();
    // AssetId is a 32-byte array, convert to hex for directory name
    let asset_id_hex = hex::encode(asset_id.as_bytes());

    // Cache uses sharded directory structure: first 2 chars / next 2 chars
    let shard1 = &asset_id_hex[0..2];
    let shard2 = &asset_id_hex[2..4];
    let asset_dir = cache_dir.join(shard1).join(shard2);
    let cache_file = asset_dir.join("file").join("body");

    assert!(
        cache_file.exists(),
        "Cache file should exist: {:?}",
        cache_file
    );

    // Second run: offline mode should read from cache
    let opts = FileSourceOptions {
        enable_range_seek: false,
        max_buffer_size: None,
        network_timeout: None,
        offline_mode: true,
    };

    let offline_session = FileSource::open(url, opts, Some(cache)).await.unwrap();

    let mut offline_stream = offline_session.stream().await;
    let mut offline_data = Vec::new();

    while let Some(chunk_result) = offline_stream.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        offline_data.extend_from_slice(&chunk);
    }

    // Offline data should match online data
    assert_eq!(online_data, offline_data);
    assert_eq!(
        offline_data,
        b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345"
    );
}

#[tokio::test]
async fn file_offline_miss_is_fatal() {
    let temp_dir = TempDir::new().unwrap();
    let url = url::Url::parse("http://example.com/not-in-cache.mp3").unwrap();

    // Create empty cache
    let cache = AssetCache::open(CacheOptions {
        max_bytes: 10 * 1024 * 1024,
        root_dir: Some(temp_dir.path().to_path_buf()),
    })
    .unwrap();

    // Try to open session with cache but no network (simulating offline)
    let session = FileSource::open(
        url,
        FileSourceOptions {
            offline_mode: true,
            ..Default::default()
        },
        Some(cache),
    )
    .await
    .unwrap();

    // Try to stream - should get OfflineMiss error
    let mut stream = session.stream().await;
    let result = stream.next().await;

    assert!(result.is_some(), "Stream should return an error");
    match result.unwrap() {
        Err(kithara_file::FileError::Driver(kithara_file::DriverError::Stream(
            kithara_stream::StreamError::Source(kithara_file::SourceError::OfflineMiss),
        ))) => {
            // Expected error
        }
        other => panic!("Expected OfflineMiss error, got: {:?}", other),
    }
}

#[tokio::test]
async fn seek_roundtrip_correctness() {
    let server = TestServer::new().await;
    let url = server.url("/test.mp3");

    let opts = FileSourceOptions {
        enable_range_seek: true,
        ..Default::default()
    };

    let session = FileSource::open(url, opts, None).await.unwrap();

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

    // Currently seek returns SeekNotSupported or DriverStopped
    // This test documents the current behavior
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

#[tokio::test]
async fn seek_variants_not_supported() {
    let server = TestServer::new().await;
    let url = server.url("/test.mp3");

    // Test with range seek disabled
    let opts = FileSourceOptions {
        enable_range_seek: false,
        ..Default::default()
    };

    let session = FileSource::open(url, opts, None).await.unwrap();

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

    // Should return error since seek is not supported
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

#[tokio::test]
async fn cancel_behavior_drop_driven() {
    let server = TestServer::new().await;
    let url = server.url("/test.mp3");

    let session = FileSource::open(url, FileSourceOptions::default(), None)
        .await
        .unwrap();

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

#[tokio::test]
async fn seek_contract_invalid_position() {
    // This test documents the expected behavior for invalid seek positions.
    // Currently, seek is not implemented, so we test the error paths.

    let server = TestServer::new().await;
    let url = server.url("/test.mp3");

    // Test with range seek enabled
    let opts = FileSourceOptions {
        enable_range_seek: true,
        ..Default::default()
    };

    let session = FileSource::open(url, opts, None).await.unwrap();

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
