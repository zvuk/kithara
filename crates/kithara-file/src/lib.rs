#![forbid(unsafe_code)]

mod driver;
mod options;
mod range_policy;
mod session;

use std::sync::Arc;

use async_trait::async_trait;
pub use driver::{DriverError, FileCommand, SourceError};
use kithara_assets::AssetStore;
use kithara_core::AssetId;
use kithara_net::{HttpClient, NetOptions};
pub use options::{FileSourceOptions, OptionsError};
pub use range_policy::RangePolicy;
pub use session::{FileError, FileResult, FileSession};
use url::Url;

/// Public contract for the progressive file source.
///
/// This trait exists to make the public API surface explicit and searchable.
/// Concrete implementations (like [`FileSource`]) must implement it.
///
/// Note: the default implementation uses [`HttpClient`] with default options.
/// If you need dependency injection for networking, that should be expressed as
/// a different constructor/implementation, not hidden behind private functions.
#[async_trait]
pub trait FileSourceContract: Send + Sync + 'static {
    async fn open(
        &self,
        url: Url,
        opts: FileSourceOptions,
        cache: Option<AssetStore>,
    ) -> FileResult<FileSession>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FileSource;

#[async_trait]
impl FileSourceContract for FileSource {
    async fn open(
        &self,
        url: Url,
        opts: FileSourceOptions,
        cache: Option<AssetStore>,
    ) -> FileResult<FileSession> {
        let asset_id = AssetId::from_url(&url)?;
        let net_client = HttpClient::new(NetOptions::default());

        let session =
            session::FileSession::new(asset_id, url, net_client, opts, cache.map(Arc::new));

        Ok(session)
    }
}

impl FileSource {
    /// Convenience associated constructor matching the historical API.
    pub async fn open(
        url: Url,
        opts: FileSourceOptions,
        cache: Option<AssetStore>,
    ) -> FileResult<FileSession> {
        FileSource.open(url, opts, cache).await
    }
}

#[cfg(test)]
mod tests {
    use axum::{Router, response::Response, routing::get};
    use bytes::Bytes;
    use futures::StreamExt;
    use kithara_assets::{EvictConfig, asset_store};
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    use super::*;

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

        let expected_asset_id = AssetId::from_url(&url).unwrap();
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
        // The stream should be a valid Stream
        // We don't test actual streaming here since that requires network access
        // This just tests that stream can be created
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

        // Read first chunk from stream
        if let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    received_data.extend_from_slice(&chunk);
                }
                Err(e) => panic!("Expected successful chunk, got error: {}", e),
            }
        }

        // Verify we got some data
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

        // Should get an error when trying to stream
        if let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(_) => panic!("Expected error, got successful chunk"),
                Err(session::FileError::Driver(driver::DriverError::Stream(
                    kithara_stream::StreamError::Source(driver::SourceError::Net(_)),
                ))) => {
                    // Expected - network error
                }
                Err(e) => panic!("Expected Network error, got: {}", e),
            }
        }
    }

    #[tokio::test]
    #[ignore = "outdated: relied on removed kithara-cache API (CacheOptions/max_bytes/root_dir); will be rewritten for kithara-assets + resource-based API"]
    async fn cache_through_write_works() {
        unimplemented!("rewrite this test for kithara-assets + resource-based API");
    }
}
