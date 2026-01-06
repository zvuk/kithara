#![forbid(unsafe_code)]

use std::time::Duration;

use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{AssetStore, EvictConfig};
use kithara_file::{DriverError, FileError, FileSource, FileSourceOptions, SourceError};
use kithara_storage::StreamingResourceExt;
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio::net::TcpListener;

async fn test_audio_endpoint() -> Response {
    // Simulate some audio data (MP3-like bytes)
    let audio_data = Bytes::from_static(b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345");
    Response::builder()
        .status(200)
        .body(axum::body::Body::from(audio_data))
        .unwrap()
}

fn test_app() -> Router {
    Router::new().route("/audio.mp3", get(test_audio_endpoint))
}

async fn run_test_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = test_app();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://127.0.0.1:{}", addr.port())
}

#[fixture]
fn example_url() -> url::Url {
    url::Url::parse("https://example.com/audio.mp3?token=123").unwrap()
}

#[fixture]
fn example_url_no_query() -> url::Url {
    url::Url::parse("https://example.com/audio.mp3").unwrap()
}

#[fixture]
fn default_opts() -> FileSourceOptions {
    FileSourceOptions::default()
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn open_session_creates_asset_id_from_url(
    example_url: url::Url,
    default_opts: FileSourceOptions,
) {
    let session = FileSource::open(example_url.clone(), default_opts, None)
        .await
        .unwrap();

    let expected_asset_id = kithara_core::AssetId::from_url(&example_url).unwrap();
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
    let session1 = FileSource::open(example_url, default_opts.clone(), None)
        .await
        .unwrap();
    let session2 = FileSource::open(example_url_no_query, default_opts, None)
        .await
        .unwrap();

    assert_eq!(session1.asset_id(), session2.asset_id());
}

#[rstest]
#[case("https://example.com/audio.mp3?token=abc")]
#[case("https://example.com/audio.mp3?different=xyz")]
#[case("https://example.com/audio.mp3")]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn asset_id_is_stable_across_different_queries(#[case] url_str: &str) {
    let url = url::Url::parse(url_str).unwrap();
    let opts = FileSourceOptions::default();

    let session = FileSource::open(url, opts, None).await.unwrap();

    // All should have same asset ID (without query)
    let expected_id =
        kithara_core::AssetId::from_url(&url::Url::parse("https://example.com/audio.mp3").unwrap())
            .unwrap();
    assert_eq!(session.asset_id(), expected_id);
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(5))]
async fn session_returns_stream(example_url_no_query: url::Url, default_opts: FileSourceOptions) {
    let session = FileSource::open(example_url_no_query, default_opts, None)
        .await
        .unwrap();

    let _stream = session.stream().await;
    // We don't consume it here; this test asserts stream construction is possible.
}

#[fixture]
async fn test_server() -> String {
    run_test_server().await
}

#[fixture]
async fn temp_assets() -> AssetStore {
    let temp_dir = TempDir::new().unwrap();
    AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default())
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn stream_bytes_from_network(
    #[future] test_server: String,
    #[future] temp_assets: AssetStore,
    default_opts: FileSourceOptions,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();
    let assets = temp_assets.await;

    let session = FileSource::open(url, default_opts, Some(assets))
        .await
        .unwrap();

    let mut stream = session.stream().await;
    let mut received_data = Vec::new();

    if let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                received_data.extend_from_slice(&chunk);
            }
            Err(e) => panic!("Expected successful chunk, got error: {e}"),
        }
    }

    assert!(!received_data.is_empty());
    assert_eq!(
        received_data,
        b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345"
    );
}

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
            // Current behavior: network failures are surfaced via the resource as a Storage failure.
            Err(FileError::Driver(DriverError::Stream(kithara_stream::StreamError::Source(
                SourceError::Storage(kithara_storage::StorageError::Failed(_)),
            )))) => {}
            Err(e) => panic!("Expected storage failure derived from network error, got: {e}"),
        }
    }
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn cache_through_write_works(
    #[future] test_server: String,
    #[future] temp_assets: AssetStore,
    default_opts: FileSourceOptions,
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();
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
    assert_eq!(
        received_data,
        b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345"
    );

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
    // kithara-file uses ResourceKey::from(&url) which creates:
    // - asset_root: first 32 chars of SHA-256 hash of URL string
    // - rel_path: last path segment of URL (or "index" if empty)
    let resource_key = kithara_assets::ResourceKey::from(&url);

    // Open the streaming resource directly to verify it exists
    let cancel = tokio_util::sync::CancellationToken::new();
    let streaming_resource = assets
        .open_streaming_resource(&resource_key, cancel.clone())
        .await
        .expect("Should be able to open cached resource");

    // Read the data from the resource using read_at since read() requires sealed state
    let stored_data = streaming_resource
        .read_at(0, received_data.len())
        .await
        .expect("Should read cached data");

    assert_eq!(stored_data, Bytes::from(received_data));
}
