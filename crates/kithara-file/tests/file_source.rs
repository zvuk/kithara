#![forbid(unsafe_code)]

use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{EvictConfig, asset_store};
use kithara_file::{DriverError, FileError, FileSource, FileSourceOptions, SourceError};
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

#[tokio::test]
async fn open_session_creates_asset_id_from_url() {
    let url = url::Url::parse("https://example.com/audio.mp3?token=123").unwrap();
    let opts = FileSourceOptions::default();

    let session = FileSource::open(url.clone(), opts, None).await.unwrap();

    let expected_asset_id = kithara_core::AssetId::from_url(&url).unwrap();
    assert_eq!(session.asset_id(), expected_asset_id);
}

#[tokio::test]
async fn asset_id_is_stable_without_query() {
    let url1 = url::Url::parse("https://example.com/audio.mp3?token=abc").unwrap();
    let url2 = url::Url::parse("https://example.com/audio.mp3?different=xyz").unwrap();
    let url3 = url::Url::parse("https://example.com/audio.mp3").unwrap();

    let session1 = FileSource::open(url1, FileSourceOptions::default(), None)
        .await
        .unwrap();
    let session2 = FileSource::open(url2, FileSourceOptions::default(), None)
        .await
        .unwrap();
    let session3 = FileSource::open(url3, FileSourceOptions::default(), None)
        .await
        .unwrap();

    assert_eq!(session1.asset_id(), session2.asset_id());
    assert_eq!(session1.asset_id(), session3.asset_id());
}

#[tokio::test]
async fn session_returns_stream() {
    let url = url::Url::parse("https://example.com/audio.mp3").unwrap();
    let opts = FileSourceOptions::default();

    let session = FileSource::open(url, opts, None).await.unwrap();

    let _stream = session.stream().await;
    // We don't consume it here; this test asserts stream construction is possible.
}

#[tokio::test]
async fn stream_bytes_from_network() {
    let server_url = run_test_server().await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let assets = asset_store(temp_dir.path().to_path_buf(), EvictConfig::default());

    let session = FileSource::open(url, FileSourceOptions::default(), Some(assets))
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

#[tokio::test]
async fn stream_handles_network_errors() {
    // Use a non-existent server to test error handling
    let url = url::Url::parse("http://127.0.0.1:9998/nonexistent.mp3").unwrap();

    let temp_dir = TempDir::new().unwrap();
    let assets = asset_store(temp_dir.path().to_path_buf(), EvictConfig::default());

    let session = FileSource::open(url, FileSourceOptions::default(), Some(assets))
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

#[tokio::test]
#[ignore = "outdated: relied on removed kithara-cache API (CacheOptions/max_bytes/root_dir); will be rewritten for kithara-assets + resource-based API"]
async fn cache_through_write_works() {
    unimplemented!("rewrite this test for kithara-assets + resource-based API");
}
