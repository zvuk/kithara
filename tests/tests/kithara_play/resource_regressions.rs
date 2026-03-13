#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

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
    play::{Resource, ResourceConfig},
};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{TestHttpServer, TestTempDir, temp_dir};

const TEST_MP3_BYTES: &[u8] = include_bytes!("../../../assets/test.mp3");
const READ_TIMEOUT: Duration = Duration::from_secs(5);

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
        .and_then(|value| value.to_str().ok())
        && let Some(range) = range_header.strip_prefix("bytes=")
    {
        let mut parts = range.split('-');
        let start = parts
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let end = parts
            .next()
            .and_then(|value| {
                if value.is_empty() {
                    None
                } else {
                    value.parse::<usize>().ok()
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

async fn ok_mp3(req: Request) -> Response {
    serve_mp3_with_range(req)
}

async fn unavailable_mp3(_req: Request) -> Response {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("temporarily unavailable"))
        .unwrap()
}

fn test_app() -> Router {
    Router::new()
        .route("/ok.mp3", get(ok_mp3).head(ok_mp3))
        .route("/gone.mp3", get(unavailable_mp3).head(unavailable_mp3))
}

fn store_options(temp_dir: &TestTempDir, ephemeral: bool) -> StoreOptions {
    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.ephemeral = true;
        store.cache_capacity = Some(NonZeroUsize::new(4).expect("nonzero"));
        store.max_assets = Some(8);
    }
    store
}

fn resource_config(url: &url::Url, store: StoreOptions) -> ResourceConfig {
    ResourceConfig::new(url.as_str())
        .unwrap()
        .with_hint("mp3")
        .with_store(store)
}

async fn open_resource(url: &url::Url, store: StoreOptions) -> Resource {
    Resource::new(resource_config(url, store))
        .await
        .unwrap_or_else(|err| panic!("resource should open for {}: {err}", url))
}

async fn read_some(resource: &mut Resource, stage: &str) -> usize {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];

    loop {
        resource.preload().await;
        let read = resource.read(&mut buf);
        if read > 0 {
            return read;
        }

        assert!(
            !resource.is_eof(),
            "unexpected EOF while waiting for stage={stage}"
        );
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for decoded PCM at stage={stage}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn seek_and_read(resource: &mut Resource, position: Duration, stage: &str) -> f64 {
    resource
        .seek(position)
        .unwrap_or_else(|err| panic!("seek must succeed at stage={stage}: {err}"));
    let read = read_some(resource, stage).await;
    assert!(read > 0, "expected decoded samples at stage={stage}");
    Resource::position(resource).as_secs_f64()
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn player_resource_repeated_unavailable_mp3_does_not_panic(temp_dir: TestTempDir) {
    let server = TestHttpServer::new(test_app()).await;
    let store = store_options(&temp_dir, true);
    let ok_url = server.url("/ok.mp3");
    let bad_url = server.url("/gone.mp3");

    let mut ok = open_resource(&ok_url, store.clone()).await;
    assert!(read_some(&mut ok, "initial_ok").await > 0);
    let forward_pos = seek_and_read(&mut ok, Duration::from_secs(2), "ok_seek_forward").await;
    assert!(
        forward_pos > 1.0,
        "forward seek should advance playback position, got {forward_pos}"
    );
    drop(ok);

    for attempt in 0..2 {
        let result = Resource::new(resource_config(&bad_url, store.clone())).await;
        assert!(
            result.is_err(),
            "unavailable resource attempt {attempt} must return error"
        );
    }

    let mut ok_again = open_resource(&ok_url, store).await;
    let replay_pos = seek_and_read(
        &mut ok_again,
        Duration::from_secs(1),
        "ok_after_unavailable_replay",
    )
    .await;
    assert!(
        replay_pos > 0.5,
        "reopened valid resource should remain seekable after failed transitions, got {replay_pos}"
    );
    assert!(
        !ok_again.is_eof(),
        "reopened valid resource must not be EOF after successful replay seek"
    );
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[cfg_attr(not(target_arch = "wasm32"), case::disk(false))]
#[case::ephemeral(true)]
async fn player_resource_mp3_reopen_same_cache_keeps_backward_seek(
    #[case] ephemeral: bool,
    temp_dir: TestTempDir,
) {
    let server = TestHttpServer::new(test_app()).await;
    let store = store_options(&temp_dir, ephemeral);
    let ok_url = server.url("/ok.mp3");

    let mut first = open_resource(&ok_url, store.clone()).await;
    assert!(read_some(&mut first, "first_initial").await > 0);
    let first_forward = seek_and_read(&mut first, Duration::from_secs(3), "first_forward").await;
    let first_backward =
        seek_and_read(&mut first, Duration::from_millis(500), "first_backward").await;
    assert!(
        first_backward < first_forward,
        "first session backward seek should move position back (forward={first_forward}, backward={first_backward})"
    );
    drop(first);

    let mut second = open_resource(&ok_url, store).await;
    assert!(read_some(&mut second, "second_initial").await > 0);
    let second_forward = seek_and_read(&mut second, Duration::from_secs(3), "second_forward").await;
    let second_backward =
        seek_and_read(&mut second, Duration::from_millis(500), "second_backward").await;
    assert!(
        second_backward < second_forward,
        "reopened session backward seek should still move position back (forward={second_forward}, backward={second_backward})"
    );
    assert!(
        !second.is_eof(),
        "reopened session must not be EOF after backward seek"
    );
}
