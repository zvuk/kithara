#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{Method, StatusCode, header},
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
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use kithara_test_utils::{TestHttpServer, TestTempDir};

const TEST_MP3_BYTES: &[u8] = include_bytes!("../../../assets/test.mp3");

#[expect(
    clippy::needless_pass_by_value,
    reason = "axum handler signature requires owned Request"
)]
fn serve_mp3_with_range(req: Request) -> Response {
    if req.method() == Method::HEAD {
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

/// Serve MP3 with real delay between chunks — simulates slow remote server.
/// Content-Length sent immediately, body drips at ~10KB/50ms.
async fn throttled_mp3_endpoint(req: Request) -> Response {
    use futures::stream::unfold;
    use kithara_platform::time::sleep;

    if req.method() == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "audio/mpeg")
            .header(header::CONTENT_LENGTH, TEST_MP3_BYTES.len().to_string())
            .body(Body::empty())
            .unwrap();
    }

    let chunk_size = 10 * 1024;
    let chunks: Vec<Bytes> = TEST_MP3_BYTES
        .chunks(chunk_size)
        .map(|c| Bytes::copy_from_slice(c))
        .collect();

    let body_stream = unfold(chunks.into_iter(), |mut iter| async move {
        let chunk = iter.next()?;
        sleep(Duration::from_millis(50)).await;
        Some((Ok::<_, std::io::Error>(chunk), iter))
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(header::CONTENT_LENGTH, TEST_MP3_BYTES.len().to_string())
        .body(Body::from_stream(body_stream))
        .unwrap()
}

fn app() -> Router {
    Router::new()
        .route("/test.mp3", get(mp3_endpoint).head(mp3_endpoint))
        // Same MP3 data served at extensionless path (like zvuk /track/streamhq?id=NNN).
        .route("/track/stream", get(mp3_endpoint).head(mp3_endpoint))
        // Throttled: Content-Length correct but body arrives in small chunks.
        .route(
            "/slow/stream",
            get(throttled_mp3_endpoint).head(throttled_mp3_endpoint),
        )
}

/// Expected duration of test.mp3 (ffprobe: 187.102041s).
const EXPECTED_DURATION_SECS: f64 = 187.0;

#[kithara::test(tokio)]
#[case::sw_ext_hint("/test.mp3", Some("mp3"), false)]
#[case::sw_ext("/test.mp3", None, false)]
#[case::sw_no_ext_hint("/track/stream", Some("mp3"), false)]
#[case::sw_no_ext("/track/stream", None, false)]
#[case::hw_ext_hint("/test.mp3", Some("mp3"), true)]
#[case::hw_no_ext("/track/stream", None, true)]
async fn audio_file_mp3_decodes_with_duration(
    #[case] path: &str,
    #[case] hint: Option<&str>,
    #[case] prefer_hardware: bool,
) {
    let server = TestHttpServer::new(app()).await;
    let temp_dir = TestTempDir::new();

    let file_config = FileConfig::new(server.url(path).into())
        .with_store(StoreOptions::new(temp_dir.path()).with_ephemeral(true));
    let mut config = AudioConfig::<File>::new(file_config).with_prefer_hardware(prefer_hardware);
    if let Some(h) = hint {
        config = config.with_hint(h);
    }
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .unwrap_or_else(|e| panic!("probe failed for path={path} hint={hint:?}: {e}"));

    // Duration must be close to 187s.
    let duration = audio.duration();
    assert!(
        duration.is_some(),
        "path={path} hint={hint:?}: duration must be reported (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        (dur_secs - EXPECTED_DURATION_SECS).abs() < 2.0,
        "path={path} hint={hint:?}: expected ~{EXPECTED_DURATION_SECS}s, got {dur_secs:.1}s"
    );

    // Decode at least 2 seconds of real PCM.
    let (samples_read, position, eof) = spawn_blocking(move || {
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
        "path={path} hint={hint:?}: playback ended too early: \
         pos={position:?} eof={eof} samples={samples_read}"
    );
}

/// Duration must be correct IMMEDIATELY after Audio::new — before any
/// decode calls. This is what the GUI reads to show track length.
///
/// Uses throttled server: Content-Length is sent immediately but body
/// arrives in small chunks, so only a fraction is downloaded when
/// the decoder initializes. Duration must still reflect the full track.
#[kithara::test(tokio, timeout(Duration::from_secs(15)))]
#[case::throttled_no_hint("/slow/stream", None)]
#[case::throttled_with_hint("/slow/stream", Some("mp3"))]
async fn mp3_duration_correct_before_decode(#[case] path: &str, #[case] hint: Option<&str>) {
    let server = TestHttpServer::new(app()).await;
    let temp_dir = TestTempDir::new();

    let file_config = FileConfig::new(server.url(path).into())
        .with_store(StoreOptions::new(temp_dir.path()).with_ephemeral(true));
    let mut config = AudioConfig::<File>::new(file_config);
    if let Some(h) = hint {
        config = config.with_hint(h);
    }
    let audio = Audio::<Stream<File>>::new(config)
        .await
        .unwrap_or_else(|e| panic!("creation failed for path={path} hint={hint:?}: {e}"));

    // Check duration BEFORE any read/decode — this is what the GUI shows.
    let duration = audio.duration();
    assert!(
        duration.is_some(),
        "path={path} hint={hint:?}: duration must be available immediately (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        (dur_secs - EXPECTED_DURATION_SECS).abs() < 2.0,
        "path={path} hint={hint:?}: expected ~{EXPECTED_DURATION_SECS}s immediately, got {dur_secs:.1}s"
    );
}

#[kithara::test(tokio)]
async fn audio_file_extensionless_mp3_without_hint_uses_native_probe() {
    let server = TestHttpServer::new(app()).await;
    let temp_dir = TestTempDir::new();

    let file_config = FileConfig::new(server.url("/get-mp3/42").into())
        .with_store(StoreOptions::new(temp_dir.path()).with_ephemeral(true));
    let config = AudioConfig::<File>::new(file_config);
    let mut audio = Audio::<Stream<File>>::new(config).await.unwrap();

    let (samples_read, position, eof) = spawn_blocking(move || {
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
        "extensionless playback ended too early: pos={position:?} eof={eof} samples={samples_read}"
    );
}
