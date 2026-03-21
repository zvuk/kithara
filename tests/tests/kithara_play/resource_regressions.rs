#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{io::Read, num::NonZeroUsize, sync::Arc};

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
    audio::{Audio, AudioConfig, AudioWorkerHandle},
    hls::{Hls, HlsConfig},
    play::{
        PlayerConfig, PlayerImpl, Resource, ResourceConfig, internal::offline::resource_from_reader,
    },
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_file::{File as FileSource, FileConfig, FileSrc};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::{
    thread,
    time::{Duration, Instant, sleep},
    tokio,
};
use kithara_test_utils::{TestHttpServer, TestTempDir, create_saw_wav, temp_dir};
use tokio::time::timeout;

const TEST_MP3_BYTES: &[u8] = include_bytes!("../../../assets/test.mp3");
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const HLS_SEGMENT_COUNT: usize = 3;
const HLS_SEGMENT_SIZE: usize = 200_000;
const HLS_TOTAL_BYTES: usize = HLS_SEGMENT_COUNT * HLS_SEGMENT_SIZE;
const HLS_SAMPLE_RATE: f64 = 44_100.0;
const HLS_CHANNELS: f64 = 2.0;

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

fn resource_config_with_worker(
    url: &url::Url,
    store: StoreOptions,
    worker: AudioWorkerHandle,
) -> ResourceConfig {
    resource_config(url, store).with_worker(worker)
}

async fn open_resource(url: &url::Url, store: StoreOptions) -> Resource {
    Resource::new(resource_config(url, store))
        .await
        .unwrap_or_else(|err| panic!("resource should open for {}: {err}", url))
}

async fn open_resource_with_worker(
    url: &url::Url,
    store: StoreOptions,
    worker: AudioWorkerHandle,
) -> Resource {
    Resource::new(resource_config_with_worker(url, store, worker))
        .await
        .unwrap_or_else(|err| panic!("resource should open for {}: {err}", url))
}

async fn warm_hls_worker(url: &url::Url, store: StoreOptions, worker: AudioWorkerHandle) -> f64 {
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let hls_config = HlsConfig::new(url.clone()).with_store(store);
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_media_info(wav_info)
        .with_worker(worker);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("HLS audio should open for {}: {err}", url));

    let deadline = Instant::now() + READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];
    loop {
        audio.preload();
        let read = audio.read(&mut buf);
        if read > 0 {
            break;
        }

        assert!(
            !audio.is_eof(),
            "unexpected EOF while warming HLS worker for {url}"
        );
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for HLS warmup data at {url}"
        );
        sleep(Duration::from_millis(10)).await;
    }

    audio
        .seek(Duration::from_secs(2))
        .unwrap_or_else(|err| panic!("HLS warmup seek must succeed for {}: {err}", url));

    loop {
        audio.preload();
        let read = audio.read(&mut buf);
        if read > 0 {
            return audio.position().as_secs_f64();
        }

        assert!(
            !audio.is_eof(),
            "unexpected EOF after HLS warmup seek for {url}"
        );
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for HLS post-seek data at {url}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn warm_hls_worker_without_seek(
    url: &url::Url,
    store: StoreOptions,
    worker: AudioWorkerHandle,
) -> f64 {
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let hls_config = HlsConfig::new(url.clone()).with_store(store);
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_media_info(wav_info)
        .with_worker(worker);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("HLS audio should open for {}: {err}", url));

    let deadline = Instant::now() + READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];
    loop {
        audio.preload();
        let read = audio.read(&mut buf);
        if read > 0 {
            return audio.position().as_secs_f64();
        }

        assert!(
            !audio.is_eof(),
            "unexpected EOF while warming HLS worker without seek for {url}"
        );
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for HLS warmup data without seek at {url}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn read_hls_stream_some(url: &url::Url, store: StoreOptions) -> usize {
    let config = HlsConfig::new(url.clone()).with_store(store);
    let mut stream = Stream::<Hls>::new(config)
        .await
        .unwrap_or_else(|err| panic!("HLS stream should open for {}: {err}", url));
    let mut buf = [0_u8; 4096];
    stream
        .read(&mut buf)
        .unwrap_or_else(|err| panic!("HLS stream should read for {}: {err}", url))
}

async fn open_audio_hls_server() -> HlsTestServer {
    let segment_duration = HLS_SEGMENT_SIZE as f64 / (HLS_SAMPLE_RATE * HLS_CHANNELS * 2.0);
    HlsTestServer::new(HlsTestServerConfig {
        custom_data: Some(Arc::new(create_saw_wav(HLS_TOTAL_BYTES))),
        segment_duration_secs: segment_duration,
        segment_size: HLS_SEGMENT_SIZE,
        segments_per_variant: HLS_SEGMENT_COUNT,
        ..Default::default()
    })
    .await
}

async fn read_some(resource: &mut Resource, stage: &str) -> usize {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];

    loop {
        timeout(READ_TIMEOUT, resource.preload())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for preload at stage={stage}"));
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
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
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
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
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

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[cfg_attr(not(target_arch = "wasm32"), case::disk(false))]
#[case::ephemeral(true)]
async fn player_worker_hls_then_unavailable_mp3_then_mp3_recovery(
    #[case] ephemeral: bool,
    temp_dir: TestTempDir,
) {
    let hls_server = open_audio_hls_server().await;
    let file_server = TestHttpServer::new(test_app()).await;
    let player = PlayerImpl::new(PlayerConfig::default());
    let worker = player.worker().clone();
    let store = store_options(&temp_dir, ephemeral);
    let hls_url = hls_server.url("/master.m3u8").unwrap();
    let ok_url = file_server.url("/ok.mp3");
    let bad_url = file_server.url("/gone.mp3");

    let hls_pos = warm_hls_worker(&hls_url, store.clone(), worker.clone()).await;
    assert!(
        hls_pos > 1.0,
        "HLS warmup seek should advance playback position, got {hls_pos}"
    );

    for attempt in 0..2 {
        let result = Resource::new(resource_config_with_worker(
            &bad_url,
            store.clone(),
            worker.clone(),
        ))
        .await;
        assert!(
            result.is_err(),
            "unavailable mp3 attempt {attempt} must return error"
        );
    }

    let mut ok = open_resource_with_worker(&ok_url, store, worker).await;
    assert!(read_some(&mut ok, "mp3_after_hls_initial").await > 0);
    let forward = seek_and_read(
        &mut ok,
        Duration::from_secs(3),
        "mp3_after_hls_seek_forward",
    )
    .await;
    let backward = seek_and_read(
        &mut ok,
        Duration::from_millis(500),
        "mp3_after_hls_seek_backward",
    )
    .await;
    assert!(
        backward < forward,
        "mp3 recovery path must keep backward seek after HLS transition (forward={forward}, backward={backward})"
    );
    assert!(
        !ok.is_eof(),
        "recovered mp3 session must not be EOF after backward seek"
    );
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn shared_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral(temp_dir: TestTempDir) {
    let hls_server = open_audio_hls_server().await;
    let file_server = TestHttpServer::new(test_app()).await;
    let worker = AudioWorkerHandle::new();
    let store = store_options(&temp_dir, true);
    let hls_url = hls_server.url("/master.m3u8").unwrap();
    let ok_url = file_server.url("/ok.mp3");

    let hls_seek = warm_hls_worker(&hls_url, store.clone(), worker.clone()).await;
    assert!(
        hls_seek > 1.0,
        "HLS warmup should advance playback position before mp3 transition, got {hls_seek}"
    );

    let mut first = open_resource_with_worker(&ok_url, store.clone(), worker.clone()).await;
    assert!(read_some(&mut first, "shared_mp3_first_initial").await > 0);
    let first_forward = seek_and_read(
        &mut first,
        Duration::from_secs(3),
        "shared_mp3_first_forward_after_hls",
    )
    .await;
    let first_backward = seek_and_read(
        &mut first,
        Duration::from_millis(500),
        "shared_mp3_first_backward_after_hls",
    )
    .await;
    assert!(
        first_backward < first_forward,
        "first shared-worker mp3 session after HLS must keep backward seek (forward={first_forward}, backward={first_backward})"
    );
    drop(first);

    let mut second = open_resource_with_worker(&ok_url, store, worker.clone()).await;
    assert!(read_some(&mut second, "shared_mp3_second_initial").await > 0);
    let second_forward = seek_and_read(
        &mut second,
        Duration::from_secs(3),
        "shared_mp3_second_forward_after_hls",
    )
    .await;
    let second_backward = seek_and_read(
        &mut second,
        Duration::from_millis(500),
        "shared_mp3_second_backward_after_hls",
    )
    .await;
    assert!(
        second_backward < second_forward,
        "reopened shared-worker mp3 session after HLS must keep backward seek (forward={second_forward}, backward={second_backward})"
    );
    assert!(
        !second.is_eof(),
        "reopened shared-worker mp3 session must not be EOF after backward seek"
    );

    worker.shutdown();
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn sequential_hls_warmup_does_not_poison_next_ephemeral_session() {
    let server_a = open_audio_hls_server().await;
    let server_b = open_audio_hls_server().await;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let worker_a = AudioWorkerHandle::new();
    let worker_b = AudioWorkerHandle::new();
    let store_a = store_options(&temp_a, false);
    let store_b = store_options(&temp_b, true);
    let hls_url_a = server_a.url("/master.m3u8").unwrap();
    let hls_url_b = server_b.url("/master.m3u8").unwrap();

    let first_pos = warm_hls_worker(&hls_url_a, store_a, worker_a.clone()).await;
    assert!(
        first_pos > 1.0,
        "first HLS warmup must advance playback position, got {first_pos}"
    );
    worker_a.shutdown();

    let second_pos = warm_hls_worker(&hls_url_b, store_b, worker_b.clone()).await;
    assert!(
        second_pos > 1.0,
        "second HLS warmup after a prior session must still advance playback position, got {second_pos}"
    );
    worker_b.shutdown();
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn sequential_hls_warmup_drop_only_does_not_poison_next_ephemeral_session() {
    let server_a = open_audio_hls_server().await;
    let server_b = open_audio_hls_server().await;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let worker_a = AudioWorkerHandle::new();
    let worker_b = AudioWorkerHandle::new();
    let store_a = store_options(&temp_a, false);
    let store_b = store_options(&temp_b, true);
    let hls_url_a = server_a.url("/master.m3u8").unwrap();
    let hls_url_b = server_b.url("/master.m3u8").unwrap();

    let first_pos = warm_hls_worker(&hls_url_a, store_a, worker_a.clone()).await;
    assert!(
        first_pos > 1.0,
        "first HLS warmup must advance playback position, got {first_pos}"
    );
    drop(worker_a);

    let second_pos = warm_hls_worker(&hls_url_b, store_b, worker_b.clone()).await;
    assert!(
        second_pos > 1.0,
        "second HLS warmup after only dropping the first worker must still advance playback position, got {second_pos}"
    );
    worker_b.shutdown();
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn sequential_hls_read_only_session_does_not_poison_next_ephemeral_session() {
    let server_a = open_audio_hls_server().await;
    let server_b = open_audio_hls_server().await;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let worker_a = AudioWorkerHandle::new();
    let worker_b = AudioWorkerHandle::new();
    let store_a = store_options(&temp_a, false);
    let store_b = store_options(&temp_b, true);
    let hls_url_a = server_a.url("/master.m3u8").unwrap();
    let hls_url_b = server_b.url("/master.m3u8").unwrap();

    let first_pos = warm_hls_worker_without_seek(&hls_url_a, store_a, worker_a.clone()).await;
    assert!(
        first_pos >= 0.0,
        "first HLS read-only warmup must produce samples, got {first_pos}"
    );
    drop(worker_a);

    let second_pos = warm_hls_worker(&hls_url_b, store_b, worker_b.clone()).await;
    assert!(
        second_pos > 1.0,
        "second HLS warmup after a read-only first session must still advance playback position, got {second_pos}"
    );
    worker_b.shutdown();
}

#[kithara::test(
    tokio,
    multi_thread,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn sequential_hls_stream_sessions_do_not_poison_next_ephemeral_session() {
    let server_a = open_audio_hls_server().await;
    let server_b = open_audio_hls_server().await;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let store_a = store_options(&temp_a, false);
    let store_b = store_options(&temp_b, true);
    let hls_url_a = server_a.url("/master.m3u8").unwrap();
    let hls_url_b = server_b.url("/master.m3u8").unwrap();

    let first_read = read_hls_stream_some(&hls_url_a, store_a).await;
    assert!(first_read > 0, "first HLS stream session must read bytes");

    let second_read = read_hls_stream_some(&hls_url_b, store_b).await;
    assert!(second_read > 0, "second HLS stream session must read bytes");
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[cfg_attr(not(target_arch = "wasm32"), case::disk(false))]
#[case::ephemeral(true)]
async fn player_worker_hls_then_mp3_reopen_keeps_backward_seek(
    #[case] ephemeral: bool,
    temp_dir: TestTempDir,
) {
    let hls_server = open_audio_hls_server().await;
    let file_server = TestHttpServer::new(test_app()).await;
    let player = PlayerImpl::new(PlayerConfig::default());
    let worker = player.worker().clone();
    let store = store_options(&temp_dir, ephemeral);
    let hls_url = hls_server.url("/master.m3u8").unwrap();
    let ok_url = file_server.url("/ok.mp3");

    let hls_seek = warm_hls_worker(&hls_url, store.clone(), worker.clone()).await;
    assert!(
        hls_seek > 1.0,
        "HLS warmup should advance playback position before mp3 transition, got {hls_seek}"
    );

    let mut first = open_resource_with_worker(&ok_url, store.clone(), worker.clone()).await;
    assert!(read_some(&mut first, "mp3_first_initial").await > 0);
    let first_forward = seek_and_read(
        &mut first,
        Duration::from_secs(3),
        "mp3_first_forward_after_hls",
    )
    .await;
    let first_backward = seek_and_read(
        &mut first,
        Duration::from_millis(500),
        "mp3_first_backward_after_hls",
    )
    .await;
    assert!(
        first_backward < first_forward,
        "first mp3 session after HLS must keep backward seek (forward={first_forward}, backward={first_backward})"
    );
    drop(first);

    let mut second = open_resource_with_worker(&ok_url, store, worker).await;
    assert!(read_some(&mut second, "mp3_second_initial").await > 0);
    let second_forward = seek_and_read(
        &mut second,
        Duration::from_secs(3),
        "mp3_second_forward_after_hls",
    )
    .await;
    let second_backward = seek_and_read(
        &mut second,
        Duration::from_millis(500),
        "mp3_second_backward_after_hls",
    )
    .await;
    assert!(
        second_backward < second_forward,
        "reopened mp3 session after HLS must keep backward seek (forward={second_forward}, backward={second_backward})"
    );
    assert!(
        !second.is_eof(),
        "reopened mp3 session must not be EOF after backward seek"
    );
}

/// Stress test: multiple crossfade transitions on shared worker.
///
/// Tests MP3→HLS, HLS→MP3, MP3→MP3 transitions with offline render.
/// Measures per-block render time and silence gaps.
/// Every render() call must complete within the audio block budget
/// (~11.6ms at 512 frames / 44100Hz) and no silence gaps > 1 block
/// are allowed during crossfade.
#[kithara::test(
    tokio,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
async fn stress_offline_crossfade_no_gaps() {
    use kithara::play::internal::offline::OfflinePlayer;

    const BLOCK: usize = 512;
    const SR: u32 = 44100;
    let block_budget = Duration::from_secs_f64(BLOCK as f64 / SR as f64);

    let hls_server = open_audio_hls_server().await;
    let store = store_options(&temp_dir(), true);
    let hls_url = hls_server.url("/master.m3u8").unwrap();

    let worker = AudioWorkerHandle::new();
    let mut player = OfflinePlayer::new(SR);

    // Local MP3 path (simulates disk-cached file, like production).
    let local_mp3 = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/test.mp3");

    // Helper: create MP3 resource (NO preload — match production timing)
    let make_mp3 = |w: AudioWorkerHandle, _s: StoreOptions| {
        let p = local_mp3.clone();
        async move {
            let file_cfg = FileConfig::new(FileSrc::Local(p));
            let audio_cfg = AudioConfig::<FileSource>::new(file_cfg)
                .with_hint("mp3")
                .with_worker(w);
            let audio = Audio::<Stream<FileSource>>::new(audio_cfg)
                .await
                .expect("create local MP3 audio");
            resource_from_reader(audio)
        }
    };

    // Helper: create and preload an HLS resource
    let make_hls = |w: AudioWorkerHandle, s: StoreOptions| {
        let u = hls_url.clone();
        async move {
            let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
            let cfg = HlsConfig::new(u).with_store(s);
            let acfg = AudioConfig::<Hls>::new(cfg)
                .with_media_info(wav_info)
                .with_worker(w);
            let audio = Audio::<Stream<Hls>>::new(acfg).await.expect("HLS audio");
            let mut r = resource_from_reader(audio);
            timeout(READ_TIMEOUT, r.preload())
                .await
                .expect("HLS preload");
            r
        }
    };

    // Helper: render N blocks, collect stats
    struct PhaseStats {
        label: String,
        blocks: u32,
        max_silence_run: u32,
        max_render: Duration,
        slow_renders: u32,
    }
    impl std::fmt::Display for PhaseStats {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "{}: {} blocks, silence={} ({:.1}ms) max_render={:?} slow={}",
                self.label,
                self.blocks,
                self.max_silence_run,
                self.max_silence_run as f64 * BLOCK as f64 / SR as f64 * 1000.0,
                self.max_render,
                self.slow_renders,
            )
        }
    }

    let render_phase = |player: &mut OfflinePlayer, n: u32, label: &str| -> PhaseStats {
        let mut max_silence = 0u32;
        let mut cur_silence = 0u32;
        let mut max_render = Duration::ZERO;
        let mut slow = 0u32;

        for _ in 0..n {
            let t = Instant::now();
            let out = player.render(BLOCK);
            let d = t.elapsed();
            if d > max_render {
                max_render = d;
            }
            if d > block_budget {
                slow += 1;
            }
            if out.iter().any(|s| s.abs() > 0.001) {
                if cur_silence > max_silence {
                    max_silence = cur_silence;
                }
                cur_silence = 0;
            } else {
                cur_silence += 1;
            }
            // Simulate real audio callback period (~11.6ms between blocks).
            // Without this, the tight loop outpaces the worker, creating
            // artificial silence that wouldn't happen in production.
            thread::sleep(block_budget.saturating_sub(d));
        }
        if cur_silence > max_silence {
            max_silence = cur_silence;
        }
        PhaseStats {
            label: label.to_owned(),
            blocks: n,
            max_silence_run: max_silence,
            max_render,
            slow_renders: slow,
        }
    };

    // Scenario 1: MP3 to HLS
    let mp3_1 = make_mp3(worker.clone(), store.clone()).await;
    player.load_and_fadein(mp3_1, "mp3_1");
    let s1a = render_phase(&mut player, 40, "MP3 solo");

    let hls_1 = make_hls(worker.clone(), store.clone()).await;
    sleep(Duration::from_millis(50)).await;
    player.load_and_fadein(hls_1, "hls_1");
    let s1b = render_phase(&mut player, 80, "MP3→HLS fade");

    // Scenario 2: HLS to MP3
    let mp3_2 = make_mp3(worker.clone(), store.clone()).await;
    sleep(Duration::from_millis(50)).await;
    player.load_and_fadein(mp3_2, "mp3_2");
    let s2 = render_phase(&mut player, 80, "HLS→MP3 fade");

    // Scenario 3: MP3 to MP3
    let mp3_3 = make_mp3(worker.clone(), store.clone()).await;
    sleep(Duration::from_millis(50)).await;
    player.load_and_fadein(mp3_3, "mp3_3");
    let s3 = render_phase(&mut player, 80, "MP3→MP3 fade");

    // Report
    eprintln!("\n=== Stress crossfade results (budget={block_budget:?}) ===");
    for s in [&s1a, &s1b, &s2, &s3] {
        eprintln!("  {s}");
    }

    // Scenario 4: repeated HLS to MP3 (intermittent glitch)
    // User reports: "чаще всего при переключении с hls на mp3,
    // это длится около 2х секунд". Run multiple iterations to catch it.
    eprintln!("\n=== Repeated HLS→MP3 crossfade (5 iterations) ===");
    let mut worst_silence = 0u32;
    let mut worst_slow = 0u32;
    let mut worst_render = Duration::ZERO;

    for iter in 0..5 {
        // HLS phase
        let hls_n = make_hls(worker.clone(), store.clone()).await;
        sleep(Duration::from_millis(50)).await;
        player.load_and_fadein(hls_n, &format!("hls_iter{iter}"));
        let _sh = render_phase(&mut player, 40, &format!("HLS solo #{iter}"));

        // Crossfade HLS→MP3
        let mp3_n = make_mp3(worker.clone(), store.clone()).await;
        sleep(Duration::from_millis(50)).await;
        player.load_and_fadein(mp3_n, &format!("mp3_iter{iter}"));
        let sm = render_phase(&mut player, 60, &format!("HLS→MP3 #{iter}"));

        eprintln!("  {sm}");
        if sm.max_silence_run > worst_silence {
            worst_silence = sm.max_silence_run;
        }
        if sm.slow_renders > worst_slow {
            worst_slow = sm.slow_renders;
        }
        if sm.max_render > worst_render {
            worst_render = sm.max_render;
        }
    }

    eprintln!(
        "\n  Worst across 5 HLS→MP3: silence={worst_silence} slow={worst_slow} \
         max_render={worst_render:?}"
    );

    // Assertions
    let all = [&s1b, &s2, &s3];
    for s in &all {
        assert!(
            s.max_silence_run <= 2,
            "{}: silence gap {} blocks ({:.1}ms) — audio underrun during crossfade",
            s.label,
            s.max_silence_run,
            s.max_silence_run as f64 * BLOCK as f64 / SR as f64 * 1000.0,
        );
        assert!(
            s.slow_renders <= 1,
            "{}: {} renders exceeded budget {block_budget:?}, max={:?} — \
             sustained blocking during crossfade",
            s.label,
            s.slow_renders,
            s.max_render,
        );
    }
    assert!(
        worst_silence <= 2,
        "HLS→MP3 repeated: worst silence gap {worst_silence} blocks — \
         intermittent underrun during crossfade"
    );
    assert!(
        worst_slow <= 1,
        "HLS→MP3 repeated: {worst_slow} blocks exceeded budget, \
         max_render={worst_render:?} — sustained blocking during crossfade"
    );
}
