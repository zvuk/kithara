//! Independent `FFmpeg` decoding of media assembled by a test.

use anyhow::{Context, Result, ensure};
use kithara::platform::time::Duration;

/// Decode the supplied container bytes without using Kithara's decoder.
pub async fn decode(bytes: &[u8]) -> Result<Vec<f32>> {
    let pcm = decode_bytes(bytes).await?;
    ensure!(
        pcm.len().is_multiple_of(4),
        "FFmpeg returned non-f32le output"
    );
    Ok(pcm
        .chunks_exact(4)
        .map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
        .collect())
}

#[cfg(target_os = "android")]
async fn decode_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let base = std::env::var("KITHARA_TEST_SERVER_URL")
        .context("Android PCM oracle requires KITHARA_TEST_SERVER_URL")?;
    let url = url::Url::parse(&base)?.join("/oracle/pcm")?;
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .post(url)
        .body(bytes.to_vec())
        .send()
        .await?;
    let status = response.status();
    let body = response.bytes().await?;
    ensure!(
        status.is_success(),
        "PCM oracle {status}: {}",
        String::from_utf8_lossy(&body)
    );
    Ok(body.to_vec())
}

#[cfg(not(target_os = "android"))]
async fn decode_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir().context("creating FFmpeg input directory")?;
    let input = directory.path().join("fragment.m4a");
    std::fs::write(&input, bytes).context("writing current media for FFmpeg")?;
    let output = kithara::platform::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&input)
            .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("FFmpeg exceeded 30 seconds")?
    .context("launching FFmpeg")?;
    ensure!(
        output.status.success(),
        "FFmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn router() -> axum::Router {
    use axum::{extract::DefaultBodyLimit, routing::post};
    axum::Router::new()
        .route("/oracle/pcm", post(serve))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
}

#[cfg(not(target_os = "android"))]
async fn serve(body: axum::body::Bytes) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};
    match decode_bytes(&body).await {
        Ok(pcm) => ([("content-type", "application/octet-stream")], pcm).into_response(),
        Err(error) => (StatusCode::UNPROCESSABLE_ENTITY, format!("{error:#}")).into_response(),
    }
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;
    use crate::TestHttpServer;

    #[kithara::test(tokio)]
    async fn remote_oracle_decodes_submitted_media_and_rejects_invalid_input() {
        let server = TestHttpServer::bind("127.0.0.1:0", router()).await;
        let url = server.base_url().join("oracle/pcm").unwrap();
        let client = reqwest::Client::new();
        let input = kithara_test_fixtures::fixtures::tone_mp3();
        let expected = decode_bytes(input).await.unwrap();
        assert!(!expected.is_empty());
        let response = client
            .post(url.clone())
            .body(input.to_vec())
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
        let response = client.post(url).body("invalid media").send().await.unwrap();
        assert_eq!(response.status(), 422);
    }
}
