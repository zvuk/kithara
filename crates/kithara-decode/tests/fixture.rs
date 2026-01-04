//! Test fixtures for decode tests.
//!
//! Provides deterministic local fixtures for decode tests (no external network).
//! Includes tiny MP3/AAC test assets embedded or served by local server.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Router,
    body::Body,
    http::{Response, StatusCode},
    routing::get,
};
use bytes::Bytes;
use tokio::net::TcpListener;
use url::Url;

/// A tiny WAV file (0.1 seconds of silence, 44.1kHz, stereo)
/// This is a minimal valid WAV file for testing.
const TINY_WAV_BYTES: &[u8] = include_bytes!("fixtures/silence_1s.wav");

/// A test MP3 file (short audio clip)
const TEST_MP3_BYTES: &[u8] = include_bytes!("fixtures/test.mp3");

/// Test server for serving audio fixtures
pub struct AudioTestServer {
    base_url: String,
    request_counts: Arc<std::sync::Mutex<HashMap<String, usize>>>,
}

impl AudioTestServer {
    /// Create a new test server
    pub async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let request_counts = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let request_counts_clone = request_counts.clone();

        let app = Router::new()
            .route("/silence.wav", get(wav_endpoint))
            .route("/test.mp3", get(mp3_endpoint))
            .layer(axum::middleware::from_fn(
                move |req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
                    let counts = request_counts_clone.clone();
                    async move {
                        let path = req.uri().path().to_string();
                        if let Ok(mut counts) = counts.lock() {
                            *counts.entry(path).or_insert(0) += 1;
                        }
                        next.run(req).await
                    }
                },
            ));

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give server time to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Self {
            base_url,
            request_counts,
        }
    }

    /// Get the base URL of the server
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get the URL for the WAV fixture
    pub fn wav_url(&self) -> Url {
        Url::parse(&format!("{}/silence.wav", self.base_url)).unwrap()
    }

    /// Get the URL for the MP3 fixture
    pub fn mp3_url(&self) -> Url {
        Url::parse(&format!("{}/test.mp3", self.base_url)).unwrap()
    }

    /// Get request count for a path
    pub fn request_count(&self, path: &str) -> usize {
        self.request_counts
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .unwrap_or(0)
    }
}

/// Handler for WAV endpoint
async fn wav_endpoint() -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "audio/wav")
        .header("Content-Length", TINY_WAV_BYTES.len().to_string())
        .body(Body::from(Bytes::from_static(TINY_WAV_BYTES)))
        .unwrap()
}

/// Handler for MP3 endpoint
async fn mp3_endpoint() -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "audio/mpeg")
        .header("Content-Length", TEST_MP3_BYTES.len().to_string())
        .body(Body::from(Bytes::from_static(TEST_MP3_BYTES)))
        .unwrap()
}

/// Embedded audio data for tests that don't need HTTP
pub struct EmbeddedAudio {
    /// WAV data (0.1 seconds of silence)
    pub wav: &'static [u8],
    /// MP3 data (test audio clip)
    pub mp3: &'static [u8],
}

impl EmbeddedAudio {
    /// Get the embedded audio data
    pub fn get() -> Self {
        Self {
            wav: TINY_WAV_BYTES,
            mp3: TEST_MP3_BYTES,
        }
    }

    /// Get WAV data
    pub fn wav(&self) -> &'static [u8] {
        self.wav
    }

    /// Get MP3 data
    pub fn mp3(&self) -> &'static [u8] {
        self.mp3
    }
}
