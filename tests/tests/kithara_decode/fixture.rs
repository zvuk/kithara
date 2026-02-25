//! Test fixtures for decode tests.
//!
//! Provides deterministic local fixtures for decode tests (no external network).
//! Includes tiny MP3/AAC test assets embedded or served by local server.

/// A tiny WAV file (0.1 seconds of silence, 44.1kHz, stereo)
/// This is a minimal valid WAV file for testing.
const TINY_WAV_BYTES: &[u8] = include_bytes!("fixtures/silence_1s.wav");

/// A test MP3 file (short audio clip)
const TEST_MP3_BYTES: &[u8] = include_bytes!("fixtures/test.mp3");

/// Embedded audio data for tests that don't need HTTP
pub(crate) struct EmbeddedAudio {
    /// WAV data (0.1 seconds of silence)
    pub(crate) wav: &'static [u8],
    /// MP3 data (test audio clip)
    pub(crate) mp3: &'static [u8],
}

impl EmbeddedAudio {
    /// Get the embedded audio data
    pub(crate) fn get() -> Self {
        Self {
            wav: TINY_WAV_BYTES,
            mp3: TEST_MP3_BYTES,
        }
    }

    /// Get WAV data
    pub(crate) fn wav(&self) -> &'static [u8] {
        self.wav
    }

    /// Get MP3 data
    pub(crate) fn mp3(&self) -> &'static [u8] {
        self.mp3
    }
}

// --- Native-only: AudioTestServer (axum) ---

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{collections::HashMap, sync::Arc};

    use axum::{
        Router,
        body::Body,
        http::{Response, StatusCode},
        routing::get,
    };
    use bytes::Bytes;
    use kithara_test_utils::TestHttpServer;
    use url::Url;

    use super::{TEST_MP3_BYTES, TINY_WAV_BYTES};

    /// Test server for serving audio fixtures
    pub(crate) struct AudioTestServer {
        server: TestHttpServer,
        request_counts: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    }

    impl AudioTestServer {
        /// Create a new test server
        pub(crate) async fn new() -> Self {
            let request_counts = Arc::new(std::sync::Mutex::new(HashMap::new()));
            let request_counts_clone = request_counts.clone();

            let app = Router::new()
                .route("/silence.wav", get(wav_endpoint))
                .route("/test.mp3", get(mp3_endpoint))
                .layer(axum::middleware::from_fn(
                    move |req: axum::http::Request<Body>, next: axum::middleware::Next| {
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

            let server = TestHttpServer::new(app).await;

            Self {
                server,
                request_counts,
            }
        }

        /// Get the base URL of the server
        #[expect(
            dead_code,
            reason = "test utility reserved for future integration tests"
        )]
        pub(crate) fn base_url(&self) -> &str {
            self.server.base_url().as_str()
        }

        /// Get the URL for the WAV fixture
        pub(crate) fn wav_url(&self) -> Url {
            self.server.url("/silence.wav")
        }

        /// Get the URL for the MP3 fixture
        pub(crate) fn mp3_url(&self) -> Url {
            self.server.url("/test.mp3")
        }

        /// Get request count for a path
        pub(crate) fn request_count(&self, path: &str) -> usize {
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
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::AudioTestServer;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use kithara_test_utils::fixture_client;
    use url::Url;

    pub(crate) struct AudioTestServer {
        session_id: String,
        base_url: Url,
    }

    impl AudioTestServer {
        pub(crate) async fn new() -> Self {
            let resp = fixture_client::create_audio_fixtures_session().await;
            Self {
                session_id: resp.session_id,
                base_url: resp.base_url.parse().unwrap(),
            }
        }

        pub(crate) fn wav_url(&self) -> Url {
            self.base_url.join("audio/silence.wav").unwrap()
        }

        pub(crate) fn mp3_url(&self) -> Url {
            self.base_url.join("audio/test.mp3").unwrap()
        }

        #[allow(dead_code)]
        pub(crate) fn request_count(&self, _path: &str) -> usize {
            0 // Not tracked in fixture server
        }
    }

    impl Drop for AudioTestServer {
        fn drop(&mut self) {
            let id = self.session_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                fixture_client::delete_session(&id).await;
            });
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::AudioTestServer;
