#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{StatusCode, header},
    response::Response,
    routing::get,
};
use bytes::Bytes;
use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
    stream::Stream,
};
use kithara_test_utils::TestHttpServer;

const TEST_MP3_BYTES: &[u8] = include_bytes!("../kithara_decode/fixtures/test.mp3");

#[expect(
    clippy::needless_pass_by_value,
    reason = "axum handler signature requires owned Request"
)]
fn serve_mp3_with_range(req: Request) -> Response {
    if req.method() == axum::http::Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "audio/mpeg")
            .header(header::CONTENT_LENGTH, TEST_MP3_BYTES.len().to_string())
            .body(Body::empty())
            .unwrap();
    }

    if let Some(range_header) = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        && let Some(range) = range_header.strip_prefix("bytes=")
    {
        let mut parts = range.split('-');
        let start = parts
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let end = parts
            .next()
            .and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    s.parse::<usize>().ok()
                }
            })
            .unwrap_or(TEST_MP3_BYTES.len().saturating_sub(1))
            .min(TEST_MP3_BYTES.len().saturating_sub(1));

        if start <= end && start < TEST_MP3_BYTES.len() {
            let chunk = &TEST_MP3_BYTES[start..=end];
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, "audio/mpeg")
                .header(header::CONTENT_LENGTH, chunk.len().to_string())
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", start, end, TEST_MP3_BYTES.len()),
                )
                .body(Body::from(Bytes::from_static(chunk)))
                .unwrap();
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(header::CONTENT_LENGTH, TEST_MP3_BYTES.len().to_string())
        .body(Body::from(Bytes::from_static(TEST_MP3_BYTES)))
        .unwrap()
}

async fn mp3_endpoint(req: Request) -> Response {
    serve_mp3_with_range(req)
}

fn app() -> Router {
    Router::new().route("/test.mp3", get(mp3_endpoint).head(mp3_endpoint))
}

#[kithara::test(tokio)]
async fn audio_file_ephemeral_mp3_does_not_end_early() {
    let server = TestHttpServer::new(app()).await;
    let temp_dir = kithara_test_utils::TestTempDir::new();

    let file_config = FileConfig::new(server.url("/test.mp3").into())
        .with_store(StoreOptions::new(temp_dir.path()).with_ephemeral(true));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut audio = Audio::<Stream<File>>::new(config).await.unwrap();

    let (samples_read, position, eof) = kithara_platform::spawn_blocking(move || {
        let mut total = 0usize;
        let mut buf = [0.0f32; 4096];

        for _ in 0..600 {
            let n = audio.read(&mut buf);
            if n == 0 {
                break;
            }
            total += n;
            if audio.position() >= Duration::from_secs(2) {
                break;
            }
        }

        (total, audio.position(), audio.is_eof())
    })
    .await
    .unwrap();

    assert!(samples_read > 0, "no decoded samples");
    assert!(
        position >= Duration::from_secs(2),
        "ephemeral playback ended too early: pos={position:?} eof={eof} samples={samples_read}"
    );
}
