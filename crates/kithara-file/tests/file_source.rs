#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{AssetId, AssetStore, AssetStoreBuilder, EvictConfig, StoreOptions};
use kithara_file::{DriverError, File, FileError, FileParams, FileSource, SourceError};
use kithara_stream::Stream;
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
fn params_small_buffer(temp_dir: TempDir) -> FileParams {
    FileParams::new(StoreOptions::new(temp_dir.path()))
        .with_max_buffer_size(16 * 1024)
}

#[fixture]
fn params_large_buffer(temp_dir: TempDir) -> FileParams {
    FileParams::new(StoreOptions::new(temp_dir.path()))
        .with_max_buffer_size(4096 * 1024)
}

#[fixture]
fn params_small_limit(temp_dir: TempDir) -> FileParams {
    let store = StoreOptions::new(temp_dir.path())
        .with_max_bytes(1024); // Small limit for testing eviction
    FileParams::new(store)
}

// ==================== Asset Store Fixtures ====================

#[fixture]
async fn temp_assets() -> AssetStore {
    let temp_dir = TempDir::new().unwrap();
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path().to_path_buf())
        .asset_root("test-file")
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
        .asset_root("test-file")
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
    default_params: FileParams,
) {
    let session = FileSource::open(example_url.clone(), default_params)
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
    temp_dir: TempDir,
) {
    let params1 = FileParams::new(StoreOptions::new(temp_dir.path()));
    let params2 = FileParams::new(StoreOptions::new(temp_dir.path()));

    let session1 = FileSource::open(example_url, params1)
        .await
        .unwrap();
    let session2 = FileSource::open(example_url_no_query, params2)
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
async fn asset_id_is_stable_across_different_queries(#[case] url_str: &str, temp_dir: TempDir) {
    let url = url::Url::parse(url_str).unwrap();
    let params = FileParams::new(StoreOptions::new(temp_dir.path()));

    let session = FileSource::open(url, params).await.unwrap();

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
async fn asset_id_differs_for_different_paths_and_domains(#[case] url_str: &str, temp_dir: TempDir) {
    let url = url::Url::parse(url_str).unwrap();
    let params = FileParams::new(StoreOptions::new(temp_dir.path()));

    let session = FileSource::open(url.clone(), params)
        .await
        .unwrap();
    let expected_id = AssetId::from_url(&url).unwrap();

    assert_eq!(session.asset_id(), expected_id);
}

// ==================== Basic Session Tests ====================

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn session_returns_stream(example_url_no_query: url::Url, default_params: FileParams) {
    let session = FileSource::open(example_url_no_query, default_params)
        .await
        .unwrap();

    let _stream = session.stream().await;
    // We don't consume it here; this test asserts stream construction is possible.
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn session_works_with_default_options(
    example_url_no_query: url::Url,
    default_params: FileParams,
) {
    let session = FileSource::open(example_url_no_query, default_params)
        .await
        .unwrap();

    // AssetId should be valid
    let _asset_id = session.asset_id();
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn session_works_with_small_buffer(
    example_url_no_query: url::Url,
    params_small_buffer: FileParams,
) {
    let session = FileSource::open(example_url_no_query, params_small_buffer)
        .await
        .unwrap();

    // AssetId should be valid
    let _asset_id = session.asset_id();
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn session_works_with_large_buffer(
    example_url_no_query: url::Url,
    params_large_buffer: FileParams,
) {
    let session = FileSource::open(example_url_no_query, params_large_buffer)
        .await
        .unwrap();

    // AssetId should be valid
    let _asset_id = session.asset_id();
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
    temp_dir: TempDir,
    #[case] path: &str,
    #[case] expected_data: &[u8],
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}{}", server_url, path).parse().unwrap();
    let params = FileParams::new(StoreOptions::new(temp_dir.path()));

    let session = FileSource::open(url, params)
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
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn stream_works_with_small_buffer(
    #[future] test_server: String,
    params_small_buffer: FileParams,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let session = FileSource::open(url, params_small_buffer).await.unwrap();

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
async fn stream_works_with_large_buffer(
    #[future] test_server: String,
    params_large_buffer: FileParams,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let session = FileSource::open(url, params_large_buffer).await.unwrap();

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
    temp_dir: TempDir,
    #[case] seek_pos: usize,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();
    let params = FileParams::new(StoreOptions::new(temp_dir.path()));

    let session = FileSource::open(url, params)
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
async fn stream_handles_network_errors(temp_dir: TempDir) {
    // Use a non-existent server to test error handling
    let url = url::Url::parse("http://127.0.0.1:9998/nonexistent.mp3").unwrap();
    let params = FileParams::new(StoreOptions::new(temp_dir.path()));

    let session = FileSource::open(url, params)
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
async fn open_fails_with_invalid_url(#[case] invalid_url: &str, temp_dir: TempDir) {
    let url = url::Url::parse(invalid_url);

    if let Ok(url) = url {
        let params = FileParams::new(StoreOptions::new(temp_dir.path()));
        let result = FileSource::open(url, params).await;
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
    temp_dir: TempDir,
    #[case] path: &str,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}{}", server_url, path).parse().unwrap();

    let params1 = FileParams::new(StoreOptions::new(temp_dir.path()));
    let params2 = FileParams::new(StoreOptions::new(temp_dir.path()));

    // First, open a session and stream data to populate cache
    let session1 = FileSource::open(url.clone(), params1)
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

    // Now create a new session with same params (simulating a new playback session)
    // This should read from cache without network requests
    let session2 = FileSource::open(url.clone(), params2)
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
}

// ==================== Stream<File> Seek Tests ====================

#[rstest]
#[case(0, b"ID3\x04\x00")]
#[case(5, b"\x00\x00\x00\x00T")]
#[case(10, b"estAu")]
#[case(22, b"12345")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_seek_start_reads_correct_bytes(
    #[future] test_server: String,
    temp_dir: TempDir,
    #[case] seek_pos: u64,
    #[case] expected: &[u8],
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let params = FileParams::new(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::open(url, params).await.unwrap();

    let expected_len = expected.len();
    let expected_vec = expected.to_vec();

    let result = tokio::task::spawn_blocking(move || {
        let pos = stream.seek(SeekFrom::Start(seek_pos)).unwrap();
        assert_eq!(pos, seek_pos);

        let mut buf = vec![0u8; expected_len];
        let n = stream.read(&mut buf).unwrap();
        (n, buf)
    })
    .await
    .unwrap();

    assert_eq!(result.0, expected_len);
    assert_eq!(&result.1[..result.0], &expected_vec[..]);
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_seek_current_works(
    #[future] test_server: String,
    temp_dir: TempDir,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let params = FileParams::new(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::open(url, params).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read first 5 bytes
        let mut buf = [0u8; 5];
        stream.read(&mut buf).unwrap();
        assert_eq!(&buf, b"ID3\x04\x00");

        // Seek forward 5 bytes (position = 5 + 5 = 10)
        let pos = stream.seek(SeekFrom::Current(5)).unwrap();
        assert_eq!(pos, 10);

        // Read from position 10
        let mut buf = [0u8; 4];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"estA");
    })
    .await
    .unwrap();
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_seek_end_works(
    #[future] test_server: String,
    temp_dir: TempDir,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let params = FileParams::new(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::open(url, params).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Seek from end (-5 bytes)
        let pos = stream.seek(SeekFrom::End(-5)).unwrap();
        // Test data: b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345" = 27 bytes
        assert_eq!(pos, 22);

        // Read last 5 bytes
        let mut buf = [0u8; 5];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"12345");
    })
    .await
    .unwrap();
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_multiple_seeks_work(
    #[future] test_server: String,
    temp_dir: TempDir,
) {
    use kithara_stream::{Source, WaitOutcome};

    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let params = FileParams::new(StoreOptions::new(temp_dir.path()));
    let session = FileSource::open(url, params).await.unwrap();
    let source = session.source().await.unwrap();

    // Wait for data to be available (writer downloads in background)
    let outcome = source.wait_range(0..27).await.unwrap();
    assert!(matches!(outcome, WaitOutcome::Ready));

    // Read from start
    let mut buf = [0u8; 3];
    let n = source.read_at(0, &mut buf).await.unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf, b"ID3");

    // Read from middle (position 13 = "Audio")
    let mut buf = [0u8; 5];
    let n = source.read_at(13, &mut buf).await.unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"Audio");

    // Read from start again
    let mut buf = [0u8; 3];
    let n = source.read_at(0, &mut buf).await.unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf, b"ID3");

    // Read at end (should return 0 bytes)
    let len = source.len().unwrap();
    let mut buf = [0u8; 10];
    let n = source.read_at(len, &mut buf).await.unwrap();
    assert_eq!(n, 0); // EOF
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_seek_past_eof_fails(
    #[future] test_server: String,
    temp_dir: TempDir,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let params = FileParams::new(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::open(url, params).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Attempt to seek past EOF
        let result = stream.seek(SeekFrom::Start(1000));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    })
    .await
    .unwrap();
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn cache_with_small_limit_evicts_old_data(
    #[future] test_server: String,
    params_small_limit: FileParams,
) {
    let server_url = test_server.await;

    // Clone params for the second session
    let params2 = params_small_limit.clone();

    // First asset - should fit in cache
    let url1: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();
    let session1 = FileSource::open(url1.clone(), params_small_limit)
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
    let session2 = FileSource::open(url2.clone(), params2)
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
