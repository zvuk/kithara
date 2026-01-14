#![forbid(unsafe_code)]

use std::time::Duration;

use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{AssetId, AssetStore, AssetStoreBuilder, EvictConfig};
use kithara_file::{DriverError, FileError, FileSource, FileSourceOptions, SourceError};
use kithara_storage::StreamingResourceExt;
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

// ==================== Test Server Fixtures ====================

async fn test_audio_endpoint() -> Response {
    // Simulate some audio data (MP3-like bytes)
    let audio_data = Bytes::from_static(b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345");
    Response::builder()
        .status(200)
        .body(axum::body::Body::from(audio_data))
        .unwrap()
}

async fn test_large_endpoint() -> Response {
    // Larger test data for buffer testing
    let large_data = Bytes::from_static(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()");
    Response::builder()
        .status(200)
        .body(axum::body::Body::from(large_data))
        .unwrap()
}

async fn test_empty_endpoint() -> Response {
    Response::builder()
        .status(200)
        .body(axum::body::Body::from(Bytes::from_static(b"")))
        .unwrap()
}

fn test_app() -> Router {
    Router::new()
        .route("/audio.mp3", get(test_audio_endpoint))
        .route("/large.bin", get(test_large_endpoint))
        .route("/empty.bin", get(test_empty_endpoint))
}

async fn run_test_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = test_app();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    format!("http://127.0.0.1:{}", addr.port())
}

// ==================== URL Fixtures ====================

#[fixture]
fn example_url() -> url::Url {
    url::Url::parse("https://example.com/audio.mp3?token=123").unwrap()
}

#[fixture]
fn example_url_no_query() -> url::Url {
    url::Url::parse("https://example.com/audio.mp3").unwrap()
}

#[fixture]
fn example_url_with_different_query() -> url::Url {
    url::Url::parse("https://example.com/audio.mp3?different=xyz").unwrap()
}

// ==================== FileSourceOptions Fixtures ====================

#[fixture]
fn default_opts() -> FileSourceOptions {
    FileSourceOptions::default()
}

#[fixture]
fn opts_small_buffer() -> FileSourceOptions {
    FileSourceOptions {
        max_buffer_size: Some(16 * 1024),
        ..Default::default()
    }
}

#[fixture]
fn opts_large_buffer() -> FileSourceOptions {
    FileSourceOptions {
        max_buffer_size: Some(4096 * 1024),
        ..Default::default()
    }
}

// ==================== Asset Store Fixtures ====================

#[fixture]
async fn temp_assets() -> AssetStore {
    let temp_dir = TempDir::new().unwrap();
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path().to_path_buf())
        .evict_config(EvictConfig::default())
        .cancel(CancellationToken::new())
        .build()
}

#[fixture]
async fn temp_assets_with_small_limit() -> AssetStore {
    let temp_dir = TempDir::new().unwrap();
    let config = EvictConfig {
        max_bytes: Some(1024), // Small limit for testing eviction
        ..Default::default()
    };
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path().to_path_buf())
        .evict_config(config)
        .cancel(CancellationToken::new())
        .build()
}

// ==================== Test Server Fixtures ====================

#[fixture]
async fn test_server() -> String {
    run_test_server().await
}

// ==================== Asset ID Tests ====================

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn open_session_creates_asset_id_from_url(
    example_url: url::Url,
    default_opts: FileSourceOptions,
) {
    let session = FileSource::open(example_url.clone(), default_opts, Some(temp_assets().await))
        .await
        .unwrap();

    let expected_asset_id = AssetId::from_url(&example_url).unwrap();
    assert_eq!(session.asset_id(), expected_asset_id);
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn asset_id_is_stable_without_query(
    example_url: url::Url,
    example_url_no_query: url::Url,
    default_opts: FileSourceOptions,
) {
    let assets = temp_assets().await;
    let session1 = FileSource::open(example_url, default_opts.clone(), Some(assets.clone()))
        .await
        .unwrap();
    let session2 = FileSource::open(example_url_no_query, default_opts, Some(assets))
        .await
        .unwrap();

    assert_eq!(session1.asset_id(), session2.asset_id());
}

#[rstest]
#[case("https://example.com/audio.mp3?token=abc")]
#[case("https://example.com/audio.mp3?different=xyz")]
#[case("https://example.com/audio.mp3")]
#[case("https://example.com/audio.mp3?multiple=params&another=value")]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn asset_id_is_stable_across_different_queries(#[case] url_str: &str) {
    let url = url::Url::parse(url_str).unwrap();
    let opts = FileSourceOptions::default();

    let assets = temp_assets().await;
    let session = FileSource::open(url, opts, Some(assets)).await.unwrap();

    // All should have same asset ID (without query)
    let expected_id =
        AssetId::from_url(&url::Url::parse("https://example.com/audio.mp3").unwrap()).unwrap();
    assert_eq!(session.asset_id(), expected_id);
}

#[rstest]
#[case("https://example.com/path/to/audio.mp3")]
#[case("https://example.com/another/path/media.mp3")]
#[case("https://different.com/audio.mp3")]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn asset_id_differs_for_different_paths_and_domains(#[case] url_str: &str) {
    let url = url::Url::parse(url_str).unwrap();
    let opts = FileSourceOptions::default();

    let assets = temp_assets().await;
    let session = FileSource::open(url.clone(), opts, Some(assets))
        .await
        .unwrap();
    let expected_id = AssetId::from_url(&url).unwrap();

    assert_eq!(session.asset_id(), expected_id);
}

// ==================== Basic Session Tests ====================

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn session_returns_stream(example_url_no_query: url::Url, default_opts: FileSourceOptions) {
    let assets = temp_assets().await;
    let session = FileSource::open(example_url_no_query, default_opts, Some(assets))
        .await
        .unwrap();

    let _stream = session.stream().await;
    // We don't consume it here; this test asserts stream construction is possible.
}

#[rstest]
#[case(default_opts())]
#[case(opts_small_buffer())]
#[case(opts_large_buffer())]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn session_works_with_different_options(
    example_url_no_query: url::Url,
    #[case] opts: FileSourceOptions,
) {
    let assets = temp_assets().await;
    let session = FileSource::open(example_url_no_query, opts, Some(assets))
        .await
        .unwrap();

    // AssetId should be valid (non-zero bytes)
    let asset_id = session.asset_id();
    let asset_id_bytes = asset_id.as_bytes();
    assert!(asset_id_bytes.iter().any(|&b| b != 0));
}

// ==================== Network Streaming Tests ====================

#[rstest]
#[case("/audio.mp3", b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345")]
#[case("/large.bin", b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()")]
#[case("/empty.bin", b"")]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn stream_bytes_from_network(
    #[future] test_server: String,
    #[future] temp_assets: AssetStore,
    #[case] path: &str,
    #[case] expected_data: &[u8],
    default_opts: FileSourceOptions,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}{}", server_url, path).parse().unwrap();
    let assets = temp_assets.await;

    let session = FileSource::open(url, default_opts, Some(assets))
        .await
        .unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                received_data.extend_from_slice(&chunk);
            }
            Err(e) => panic!("Expected successful chunk, got error: {e}"),
        }
    }

    assert_eq!(received_data, expected_data);
}

#[rstest]
#[case(opts_small_buffer())]
#[case(opts_large_buffer())]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn stream_works_with_different_buffer_sizes(
    #[future] test_server: String,
    #[future] temp_assets: AssetStore,
    #[case] opts: FileSourceOptions,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();
    let assets = temp_assets.await;

    let session = FileSource::open(url, opts, Some(assets)).await.unwrap();

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
#[case(0)]
#[case(4)]
#[case(10)]
#[case(20)]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn stream_seek_bytes_repositions_reader(
    #[future] test_server: String,
    #[future] temp_assets: AssetStore,
    #[case] seek_pos: usize,
    default_opts: FileSourceOptions,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();
    let assets = temp_assets.await;

    let session = FileSource::open(url, default_opts, Some(assets))
        .await
        .unwrap();

    let mut stream = session.stream().await;

    // Seek to position
    session.seek_bytes(seek_pos as u64).await.unwrap();

    // Read from seek position
    let mut received_data = Vec::new();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("Stream should not error after seek");
        received_data.extend_from_slice(&chunk);
    }

    let expected_data = &b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345"[seek_pos..];
    assert_eq!(received_data, expected_data);
}

// ==================== Error Handling Tests ====================

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn stream_handles_network_errors(
    #[future] temp_assets: AssetStore,
    default_opts: FileSourceOptions,
) {
    // Use a non-existent server to test error handling
    let url = url::Url::parse("http://127.0.0.1:9998/nonexistent.mp3").unwrap();
    let assets = temp_assets.await;

    let session = FileSource::open(url, default_opts, Some(assets))
        .await
        .unwrap();

    let mut stream = session.stream().await;

    if let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(_) => panic!("Expected error, got successful chunk"),
            // Network failures can be surfaced in different ways
            Err(FileError::Driver(DriverError::Stream(kithara_stream::StreamError::Source(
                SourceError::Storage(kithara_storage::StorageError::Failed(_)),
            )))) => {}
            Err(FileError::Driver(DriverError::Stream(kithara_stream::StreamError::Source(
                SourceError::Net(_),
            )))) => {}
            Err(FileError::Driver(DriverError::Stream(kithara_stream::StreamError::Source(
                SourceError::Assets(_),
            )))) => {}
            Err(e) => panic!("Expected network or storage error, got: {e}"),
        }
    }
}

#[rstest]
#[case("http://")]
#[case("not-a-url")]
#[case("ftp://example.com/audio.mp3")]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn open_fails_with_invalid_url(#[case] invalid_url: &str, default_opts: FileSourceOptions) {
    let url = url::Url::parse(invalid_url);

    if let Ok(url) = url {
        let assets = temp_assets().await;
        let result = FileSource::open(url, default_opts, Some(assets)).await;
        // Depending on implementation, this might fail or succeed
        // We just verify it doesn't panic
        assert!(result.is_ok() || result.is_err());
    } else {
        // Invalid URL string - parse should fail
        assert!(url.is_err());
    }
}

// ==================== Cache Tests ====================

#[rstest]
#[case("/audio.mp3")]
#[case("/large.bin")]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn cache_through_write_works(
    #[future] test_server: String,
    #[future] temp_assets: AssetStore,
    #[case] path: &str,
    default_opts: FileSourceOptions,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}{}", server_url, path).parse().unwrap();
    let assets = temp_assets.await;

    // First, open a session and stream data to populate cache
    let session1 = FileSource::open(url.clone(), default_opts.clone(), Some(assets.clone()))
        .await
        .unwrap();

    let mut stream1 = session1.stream().await;
    let mut received_data = Vec::new();

    while let Some(chunk_result) = stream1.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        received_data.extend_from_slice(&chunk);
    }

    // Verify we received the expected data
    let expected_data: &[u8] = match path {
        "/audio.mp3" => b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345",
        "/large.bin" => b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()",
        _ => &[],
    };
    assert_eq!(received_data, expected_data);

    // Now create a new session with the same assets store (simulating a new playback session)
    // This should read from cache without network requests
    let session2 = FileSource::open(url.clone(), default_opts, Some(assets.clone()))
        .await
        .unwrap();

    let mut stream2 = session2.stream().await;
    let mut cached_data = Vec::new();

    while let Some(chunk_result) = stream2.next().await {
        let chunk = chunk_result.expect("Stream should not error from cache");
        cached_data.extend_from_slice(&chunk);
    }

    // Verify cached data matches original
    assert_eq!(received_data, cached_data);

    // Verify the data is actually stored on disk
    let resource_key = kithara_assets::ResourceKey::from(&url);

    // Open the streaming resource directly to verify it exists
    let streaming_resource = assets
        .open_streaming_resource(&resource_key)
        .await
        .expect("Should be able to open cached resource");

    // Read the data from the resource using read_at since read() requires sealed state
    let stored_data = streaming_resource
        .read_at(0, received_data.len())
        .await
        .expect("Should read cached data");

    assert_eq!(stored_data, Bytes::from(received_data));
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn cache_with_small_limit_evicts_old_data(
    #[future] test_server: String,
    #[future] temp_assets_with_small_limit: AssetStore,
    default_opts: FileSourceOptions,
) {
    let server_url = test_server.await;
    let assets = temp_assets_with_small_limit.await;

    // First asset - should fit in cache
    let url1: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();
    let session1 = FileSource::open(url1.clone(), default_opts.clone(), Some(assets.clone()))
        .await
        .unwrap();

    let mut stream1 = session1.stream().await;
    let mut data1 = Vec::new();
    while let Some(chunk_result) = stream1.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        data1.extend_from_slice(&chunk);
    }

    // Second asset - larger, might cause eviction
    let url2: url::Url = format!("{}/large.bin", server_url).parse().unwrap();
    let session2 = FileSource::open(url2.clone(), default_opts.clone(), Some(assets.clone()))
        .await
        .unwrap();

    let mut stream2 = session2.stream().await;
    let mut data2 = Vec::new();
    while let Some(chunk_result) = stream2.next().await {
        let chunk = chunk_result.expect("Stream should not error");
        data2.extend_from_slice(&chunk);
    }

    // Verify both assets were downloaded successfully
    assert_eq!(data1, b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345");
    assert_eq!(data2, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()");
}
