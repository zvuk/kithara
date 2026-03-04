//! HTTP client for creating/deleting fixture server sessions.
//!
//! Works on both native and WASM targets via `reqwest`.

#[cfg(not(target_arch = "wasm32"))]
use std::env;

use crate::fixture_protocol::{
    AbrSessionConfig, AudioFixturesSessionConfig, FileSessionConfig, FixedHlsSessionConfig,
    HlsSessionConfig, HttpTestSessionConfig, SessionResponse,
};

/// Get the fixture server base URL from environment or default.
#[must_use]
pub fn fixture_server_url() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        env::var("FIXTURE_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:3333".to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        // On WASM, env vars are not available at runtime.
        // Use compile-time env or default.
        option_env!("FIXTURE_SERVER_URL")
            .unwrap_or("http://127.0.0.1:3333")
            .to_string()
    }
}

/// Parse a JSON response body into `SessionResponse`.
async fn parse_response(resp: reqwest::Response) -> SessionResponse {
    let text = resp.text().await.expect("fixture server: read body");
    serde_json::from_str(&text).expect("fixture server: parse SessionResponse")
}

/// Create a fixed HLS session (maps to `TestServer`).
pub async fn create_fixed_hls_session() -> SessionResponse {
    let base = fixture_server_url();
    let body = serde_json::to_string(&FixedHlsSessionConfig).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/session/hls-fixed"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .expect("fixture server: create fixed HLS session")
        .error_for_status()
        .expect("fixture server: bad status for fixed HLS session");
    parse_response(resp).await
}

/// Create a configurable HLS session (maps to `HlsTestServer`).
pub async fn create_hls_session(config: &HlsSessionConfig) -> SessionResponse {
    let base = fixture_server_url();
    let body = serde_json::to_string(config).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/session/hls"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .expect("fixture server: create HLS session")
        .error_for_status()
        .expect("fixture server: bad status for HLS session");
    parse_response(resp).await
}

/// Create an ABR test session (maps to `AbrTestServer`).
pub async fn create_abr_session(config: &AbrSessionConfig) -> SessionResponse {
    let base = fixture_server_url();
    let body = serde_json::to_string(config).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/session/abr"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .expect("fixture server: create ABR session")
        .error_for_status()
        .expect("fixture server: bad status for ABR session");
    parse_response(resp).await
}

/// Create an audio fixtures session (serves silence.wav and test.mp3).
pub async fn create_audio_fixtures_session() -> SessionResponse {
    let base = fixture_server_url();
    let body = serde_json::to_string(&AudioFixturesSessionConfig).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/session/audio-fixtures"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .expect("fixture server: create audio fixtures session")
        .error_for_status()
        .expect("fixture server: bad status for audio fixtures session");
    parse_response(resp).await
}

/// Create an HTTP test session (generic endpoint testing).
pub async fn create_http_test_session(config: &HttpTestSessionConfig) -> SessionResponse {
    let base = fixture_server_url();
    let body = serde_json::to_string(config).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/session/http-test"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .expect("fixture server: create HTTP test session")
        .error_for_status()
        .expect("fixture server: bad status for HTTP test session");
    parse_response(resp).await
}

/// Create a file download test session.
pub async fn create_file_session(config: &FileSessionConfig) -> SessionResponse {
    let base = fixture_server_url();
    let body = serde_json::to_string(config).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/session/file"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .expect("fixture server: create file session")
        .error_for_status()
        .expect("fixture server: bad status for file session");
    parse_response(resp).await
}

/// Delete a session (best-effort, errors are ignored).
pub async fn delete_session(session_id: &str) {
    let base = fixture_server_url();
    let _ = reqwest::Client::new()
        .delete(format!("{base}/session/{session_id}"))
        .send()
        .await;
}
