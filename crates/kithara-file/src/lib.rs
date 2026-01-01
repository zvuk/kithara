#![forbid(unsafe_code)]

use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_core::AssetId;
use kithara_net::{NetClient, NetOptions};
use std::pin::Pin;
#[cfg(feature = "cache")]
use std::sync::Arc;
use thiserror::Error;
use url::Url;

#[cfg(feature = "cache")]
use kithara_cache::{AssetCache, CachePath};

#[derive(Debug, Error)]
pub enum FileError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),
    #[cfg(feature = "cache")]
    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),
    #[error("not implemented")]
    Unimplemented,
}

pub type FileResult<T> = Result<T, FileError>;

#[derive(Clone, Debug, Default)]
pub struct FileSourceOptions;

#[derive(Debug)]
pub enum FileCommand {
    Stop,
    SeekBytes(u64),
}

#[derive(Debug)]
pub struct FileSource;

impl FileSource {
    pub async fn open(url: Url, _opts: FileSourceOptions) -> FileResult<FileSession> {
        let asset_id = AssetId::from_url(&url);
        let net_client = NetClient::new(kithara_net::NetOptions::default());
        
        let session = FileSession {
            asset_id,
            url,
            net_client,
            #[cfg(feature = "cache")]
            cache: None,
        };
        
        Ok(session)
    }

    #[cfg(feature = "cache")]
    pub async fn open_with_cache(
        url: Url, 
        _opts: FileSourceOptions,
        cache: Option<AssetCache>
    ) -> FileResult<FileSession> {
        let asset_id = AssetId::from_url(&url);
        let net_client = NetClient::new(kithara_net::NetOptions::default());
        
        let session = FileSession {
            asset_id,
            url,
            net_client,
            cache: cache.map(Arc::new),
        };
        
        Ok(session)
    }
}

#[derive(Debug, Clone)]
pub struct FileSession {
    asset_id: AssetId,
    url: Url,
    net_client: NetClient,
    #[cfg(feature = "cache")]
    cache: Option<Arc<AssetCache>>,
}

impl FileSession {
    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub fn stream(&self) -> Pin<Box<dyn Stream<Item = FileResult<Bytes>> + Send + '_>> {
        let client = self.net_client.clone();
        let url = self.url.clone();
        
        #[cfg(feature = "cache")]
        let cache = self.cache.clone();
        
        Box::pin(stream! {
            // Check cache first if available
            #[cfg(feature = "cache")]
            if let Some(ref cache) = cache {
                let asset_handle = cache.asset(self.asset_id);
                let body_path = match CachePath::new(vec!["file".to_string(), "body".to_string()]) {
                    Ok(path) => path,
                    Err(e) => {
                        yield Err(FileError::Cache(e));
                        return;
                    }
                };
                
                if let Some(mut file) = asset_handle.open(&body_path).unwrap_or(None) {
                    use std::io::Read;
                    let mut buffer = vec![0u8; 8192];
                    loop {
                        match file.read(&mut buffer) {
                            Ok(0) => return, // EOF
                            Ok(n) => {
                                yield Ok(Bytes::copy_from_slice(&buffer[..n]));
                            }
                            Err(e) => {
                                yield Err(FileError::Cache(kithara_cache::CacheError::Io(e)));
                                return;
                            }
                        }
                    }
                }
            }
            
            // Stream from network
            let mut stream = match client.stream(url, None).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(FileError::Net(e));
                    return;
                }
            };
            
            #[cfg(feature = "cache")]
            let mut cached_bytes = Vec::new();
            
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        #[cfg(feature = "cache")]
                        if let Some(ref _cache) = cache {
                            cached_bytes.extend_from_slice(&bytes);
                        }
                        
                        yield Ok(bytes);
                    }
                    Err(e) => {
                        yield Err(FileError::Net(e));
                        break;
                    }
                }
            }
            
            // Write to cache after successful download
            #[cfg(feature = "cache")]
            if let Some(ref cache) = cache && !cached_bytes.is_empty() {
                let asset_handle = cache.asset(self.asset_id);
                let body_path = match CachePath::new(vec!["file".to_string(), "body".to_string()]) {
                    Ok(path) => path,
                    Err(_e) => {
                        // Cache write failure shouldn't affect stream result
                        return;
                    }
                };
                
                let _ = asset_handle.put_atomic(&body_path, &cached_bytes);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{response::Response, routing::get, Router};
    use bytes::Bytes;
    use futures::StreamExt;
    use tokio::net::TcpListener;

    async fn test_audio_endpoint() -> Response {
        // Simulate some audio data (MP3-like bytes)
        let audio_data = Bytes::from_static(b"ID3\x04\x00\x00\x00\x00\x00\x00TestAudioData12345");
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
        
        let session = FileSource::open(url.clone(), opts).await.unwrap();
        
        let expected_asset_id = AssetId::from_url(&url);
        assert_eq!(session.asset_id(), expected_asset_id);
    }

    #[tokio::test]
    async fn asset_id_is_stable_without_query() {
        let url1 = url::Url::parse("https://example.com/audio.mp3?token=abc").unwrap();
        let url2 = url::Url::parse("https://example.com/audio.mp3?different=xyz").unwrap();
        let url3 = url::Url::parse("https://example.com/audio.mp3").unwrap();
        
        let session1 = FileSource::open(url1, FileSourceOptions::default()).await.unwrap();
        let session2 = FileSource::open(url2, FileSourceOptions::default()).await.unwrap();
        let session3 = FileSource::open(url3, FileSourceOptions::default()).await.unwrap();
        
        assert_eq!(session1.asset_id(), session2.asset_id());
        assert_eq!(session1.asset_id(), session3.asset_id());
    }

    #[tokio::test]
    async fn session_returns_stream() {
        let url = url::Url::parse("https://example.com/audio.mp3").unwrap();
        let opts = FileSourceOptions::default();
        
        let session = FileSource::open(url, opts).await.unwrap();
        
        let _stream = session.stream();
        // The stream should be a valid Stream
        // We don't test actual streaming here since that requires network access
        // This just tests that the stream can be created
    }

    #[tokio::test]
    async fn stream_bytes_from_network() {
        let server_url = run_test_server().await;
        let url: url::Url = format!("{}/audio.mp3", server_url)
            .parse()
            .unwrap();
        
        let session = FileSource::open(url, FileSourceOptions::default())
            .await
            .unwrap();
        
        let mut stream = session.stream();
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
        assert_eq!(received_data, b"ID3\x04\x00\x00\x00\x00\x00\x00TestAudioData12345");
    }

    #[tokio::test]
    async fn stream_handles_network_errors() {
        // Use a non-existent server to test error handling
        let url = url::Url::parse("http://127.0.0.1:9998/nonexistent.mp3")
            .unwrap();
        
        let session = FileSource::open(url, FileSourceOptions::default())
            .await
            .unwrap();
        
        let mut stream = session.stream();
        
        // Should get an error when trying to stream
        if let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(_) => panic!("Expected error, got successful chunk"),
                Err(FileError::Net(_)) => {
                    // Expected - network error
                }
                Err(e) => panic!("Expected Network error, got: {}", e),
            }
        }
    }

    #[cfg(feature = "cache")]
    #[tokio::test]
    async fn cache_through_write_works() {
        use kithara_cache::{CacheOptions, CachePath};
        use std::time::Duration;
        
        let server_url = run_test_server().await;
        let url: url::Url = format!("{}/audio.mp3", server_url)
            .parse()
            .unwrap();
        
        // Create cache and download
        let cache = AssetCache::open(CacheOptions {
            max_bytes: 10 * 1024 * 1024, // 10MB
            root_dir: None,
        }).unwrap();
        
        let session = FileSource::open_with_cache(url, FileSourceOptions::default(), Some(cache))
            .await
            .unwrap();
        
        let mut stream = session.stream();
        let mut received_data = Vec::new();
        
        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => received_data.extend_from_slice(&chunk),
                Err(e) => panic!("Expected successful chunk, got error: {}", e),
            }
        }
        
        // Verify download worked
        assert!(!received_data.is_empty());
        assert_eq!(received_data, b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345");
        
        // Verify data was written to cache after short delay
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        let asset_id = AssetId::from_url(&url);
        let asset_handle = cache.asset(asset_id);
        let body_path = CachePath::new(vec!["file".to_string(), "body".to_string()]).unwrap();
        
        // File should exist in cache now
        assert!(asset_handle.exists(&body_path));
        
        // Should be able to read from cache
        if let Some(file) = asset_handle.open(&body_path).unwrap() {
            use std::io::Read;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer).unwrap();
            assert_eq!(buffer, received_data);
        } else {
            panic!("File not found in cache");
        }
    }
        }
    }
        }
        
        // Should read from cache and get same data
        assert_eq!(received_data2, received_data1);
    }
}