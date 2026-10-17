//! Independent `FFmpeg` decoding of media assembled by a test.

use anyhow::{Context, Result, ensure};
pub use cochlea_features::Audio;
use kithara::platform::time::Duration;

/// Decode the supplied container bytes without using Kithara's decoder.
pub async fn decode(bytes: &[u8]) -> Result<Vec<f32>> {
    decode_samples(&decode_bytes(bytes).await?)
}

fn decode_samples(pcm: &[u8]) -> Result<Vec<f32>> {
    ensure!(
        pcm.len().is_multiple_of(4),
        "FFmpeg returned non-f32le output"
    );
    Ok(pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
        .collect())
}

/// Decode one declared presentation at its independently probed native audio spec.
///
/// Container edits and codec priming do not establish a raw-to-media origin here.
///
/// # Errors
/// Returns an error when probing or decoding fails, the stream is ambiguous,
/// its declared duration is not an exact native frame count, or its decoded
/// samples do not form nonempty finite audio at the probed spec.
#[cfg(not(target_os = "android"))]
#[kithara::flash(io)]
pub async fn decode_native(bytes: &[u8]) -> Result<Audio> {
    let directory = tempfile::tempdir().context("creating FFprobe input directory")?;
    let input = directory.path().join("fragment.m4a");
    std::fs::write(&input, bytes).context("writing current media for FFprobe")?;
    let output = kithara::platform::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=sample_rate,channels,time_base,duration_ts",
                "-of",
                "json",
            ])
            .arg(&input)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("FFprobe exceeded 30 seconds")?
    .context("launching FFprobe")?;
    ensure!(
        output.status.success(),
        "FFprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing FFprobe audio metadata")?;
    let streams = metadata
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .context("FFprobe omitted its audio streams")?;
    let [stream] = streams.as_slice() else {
        anyhow::bail!("expected one audio stream, FFprobe found {}", streams.len());
    };
    let sample_rate = stream
        .get("sample_rate")
        .and_then(serde_json::Value::as_str)
        .context("FFprobe omitted the native sample rate")?
        .parse::<u32>()
        .context("FFprobe returned an invalid native sample rate")?;
    let channels = stream
        .get("channels")
        .and_then(serde_json::Value::as_u64)
        .context("FFprobe omitted the native channel count")?;
    let channels = u16::try_from(channels).context("FFprobe channel count exceeds u16")?;
    ensure!(
        sample_rate > 0 && channels > 0,
        "FFprobe returned an empty audio spec"
    );
    let frames = presentation_frames(stream, sample_rate)?;
    let samples = decode_samples(&decode_file(&input, Some(frames)).await?)?;
    ensure!(!samples.is_empty(), "FFmpeg returned no audio samples");
    ensure!(
        samples.len().is_multiple_of(usize::from(channels)),
        "FFmpeg returned a partial audio frame"
    );
    ensure!(
        samples.iter().all(|sample| sample.is_finite()),
        "FFmpeg returned non-finite audio"
    );
    Ok(Audio {
        samples,
        channels,
        sample_rate,
    })
}

#[cfg(not(target_os = "android"))]
fn presentation_frames(stream: &serde_json::Value, sample_rate: u32) -> Result<u64> {
    let duration = stream
        .get("duration_ts")
        .and_then(serde_json::Value::as_u64)
        .context("FFprobe omitted a nonnegative presentation duration")?;
    let (numerator, denominator) = stream
        .get("time_base")
        .and_then(serde_json::Value::as_str)
        .and_then(|base| base.split_once('/'))
        .context("FFprobe omitted a rational time base")?;
    let numerator = numerator
        .parse::<u64>()
        .context("invalid time-base numerator")?;
    let denominator = denominator
        .parse::<u64>()
        .context("invalid time-base denominator")?;
    ensure!(
        numerator > 0 && denominator > 0,
        "FFprobe returned a nonpositive time base"
    );
    let scaled = u128::from(duration)
        .checked_mul(u128::from(numerator))
        .and_then(|ticks| ticks.checked_mul(u128::from(sample_rate)))
        .context("FFprobe presentation frame count overflowed")?;
    ensure!(
        scaled > 0 && scaled.is_multiple_of(u128::from(denominator)),
        "FFprobe presentation duration is not a positive native frame count"
    );
    u64::try_from(scaled / u128::from(denominator))
        .context("FFprobe presentation frame count exceeds u64")
}

#[cfg(target_os = "android")]
#[kithara::flash(io)]
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
#[kithara::flash(io)]
async fn decode_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir().context("creating FFmpeg input directory")?;
    let input = directory.path().join("fragment.m4a");
    std::fs::write(&input, bytes).context("writing current media for FFmpeg")?;
    decode_file(&input, None).await
}

#[cfg(not(target_os = "android"))]
async fn decode_file(input: &std::path::Path, presentation_frames: Option<u64>) -> Result<Vec<u8>> {
    let mut command = tokio::process::Command::new("ffmpeg");
    command
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input);
    if let Some(frames) = presentation_frames {
        command.args(["-af", &format!("atrim=end_sample={frames}")]);
    }
    let output = kithara::platform::time::timeout(
        Duration::from_secs(30),
        command
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
    use kithara_test_utils::TestHttpServer;

    use super::*;

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
