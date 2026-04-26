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
    audio::{Audio, AudioConfig, AudioWorkerHandle, ReadOutcome},
    hls::{Hls, HlsConfig},
    net::NetOptions,
    play::{
        PlayerConfig, PlayerImpl, Resource, ResourceConfig, internal::offline::resource_from_reader,
    },
    stream::{
        AudioCodec, ContainerFormat, MediaInfo, Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_file::{File as FileSource, FileConfig, FileSrc};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{
    HlsFixtureBuilder, TestHttpServer, TestServerHelper, TestTempDir, create_wav_exact_bytes,
    fixture_protocol::PackagedSignal, signal_pcm::signal, temp_dir,
};
use tokio::time::timeout;
use tracing::{debug, info};

use crate::{
    common::{decoder_backend::DecoderBackend, test_defaults::Consts as Shared},
    continuity::{
        CONTINUITY_BLOCK_FRAMES, CONTINUITY_SAMPLE_RATE, PlaybackProgressProbe,
        render_offline_window,
    },
};

struct Consts;
impl Consts {
    const TEST_MP3_BYTES: &'static [u8] = Shared::TEST_MP3_BYTES;
    const READ_TIMEOUT: Duration = Shared::READ_TIMEOUT;
    const HLS_SEGMENT_COUNT: usize = 3;
    const HLS_SEGMENT_SIZE: usize = Shared::SEGMENT_SIZE;
    const HLS_TOTAL_BYTES: usize = Self::HLS_SEGMENT_COUNT * Self::HLS_SEGMENT_SIZE;
    const HLS_SAMPLE_RATE: f64 = Shared::SAMPLE_RATE as f64;
    const HLS_CHANNELS: f64 = Shared::CHANNELS as f64;
    /// Expected duration of test.mp3 (ffprobe: 187.102041s).
    const EXPECTED_DURATION_SECS: f64 = Shared::TEST_MP3_DURATION_SECS;
}

fn packaged_single_variant_builder(codec: AudioCodec) -> HlsFixtureBuilder {
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(8)
        .segment_duration_secs(0.5);
    match codec {
        AudioCodec::AacLc => builder.packaged_audio_signal_aac_lc(
            CONTINUITY_SAMPLE_RATE,
            2,
            PackagedSignal::Sawtooth,
        ),
        AudioCodec::Flac => {
            builder.packaged_audio_signal_flac(CONTINUITY_SAMPLE_RATE, 2, PackagedSignal::Sawtooth)
        }
        other => panic!("unsupported packaged single-variant codec: {other:?}"),
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "axum handler signature requires owned Request"
)]
fn serve_mp3_with_range(req: Request) -> Response {
    if req.method() == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "audio/mpeg")
            .header(
                header::CONTENT_LENGTH,
                Consts::TEST_MP3_BYTES.len().to_string(),
            )
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
            .unwrap_or(Consts::TEST_MP3_BYTES.len().saturating_sub(1))
            .min(Consts::TEST_MP3_BYTES.len().saturating_sub(1));

        if start <= end && start < Consts::TEST_MP3_BYTES.len() {
            let chunk = &Consts::TEST_MP3_BYTES[start..=end];
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, "audio/mpeg")
                .header(header::CONTENT_LENGTH, chunk.len().to_string())
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", start, end, Consts::TEST_MP3_BYTES.len()),
                )
                .body(Body::from(Bytes::from_static(chunk)))
                .unwrap();
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(
            header::CONTENT_LENGTH,
            Consts::TEST_MP3_BYTES.len().to_string(),
        )
        .body(Body::from(Bytes::from_static(Consts::TEST_MP3_BYTES)))
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
        // Same MP3 served at extensionless path (like zvuk /track/streamhq?id=NNN).
        .route("/track/stream", get(ok_mp3).head(ok_mp3))
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

/// Build a `ResourceConfig` with the common shape used throughout this
/// file: backend-preferred hardware flag, optional MP3 hint, optional
/// shared audio worker handle.
fn resource_config(
    url: &url::Url,
    store: StoreOptions,
    backend: DecoderBackend,
    hint: Option<&str>,
    worker: Option<AudioWorkerHandle>,
) -> ResourceConfig {
    let mut cfg = ResourceConfig::new(url.as_str()).unwrap().with_store(store);
    if let Some(h) = hint {
        cfg = cfg.with_hint(h);
    }
    if let Some(w) = worker {
        cfg = cfg.with_worker(w);
    }
    cfg.prefer_hardware = backend.prefer_hardware();
    cfg
}

/// Open a resource with [`resource_config`] options; panics on error.
async fn open_resource_full(
    url: &url::Url,
    store: StoreOptions,
    backend: DecoderBackend,
    hint: Option<&str>,
    worker: Option<AudioWorkerHandle>,
) -> Resource {
    Resource::new(resource_config(url, store, backend, hint, worker))
        .await
        .unwrap_or_else(|err| panic!("resource should open for {}: {err}", url))
}

async fn open_resource(url: &url::Url, store: StoreOptions, backend: DecoderBackend) -> Resource {
    open_resource_full(url, store, backend, Some("mp3"), None).await
}

async fn open_resource_with_worker(
    url: &url::Url,
    store: StoreOptions,
    worker: AudioWorkerHandle,
    backend: DecoderBackend,
) -> Resource {
    open_resource_full(url, store, backend, Some("mp3"), Some(worker)).await
}

fn resource_config_with_worker(
    url: &url::Url,
    store: StoreOptions,
    worker: AudioWorkerHandle,
    backend: DecoderBackend,
) -> ResourceConfig {
    resource_config(url, store, backend, Some("mp3"), Some(worker))
}

fn resource_config_no_hint(
    url: &url::Url,
    store: StoreOptions,
    backend: DecoderBackend,
) -> ResourceConfig {
    resource_config(url, store, backend, None, None)
}

async fn warm_hls_worker(
    url: &url::Url,
    store: StoreOptions,
    worker: AudioWorkerHandle,
    backend: DecoderBackend,
) -> f64 {
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let hls_config = HlsConfig::new(url.clone()).with_store(store);
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_media_info(wav_info)
        .with_worker(worker)
        .with_prefer_hardware(backend.prefer_hardware());
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("HLS audio should open for {}: {err}", url));

    let deadline = Instant::now() + Consts::READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];
    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count > 0 => break,
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while warming HLS worker for {url}")
            }
            Err(e) => panic!("decode error while warming HLS worker for {url}: {e}"),
        }
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
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count > 0 => {
                return audio.position().as_secs_f64();
            }
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF after HLS warmup seek for {url}")
            }
            Err(e) => panic!("decode error after HLS warmup seek for {url}: {e}"),
        }
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
    backend: DecoderBackend,
) -> f64 {
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let hls_config = HlsConfig::new(url.clone()).with_store(store);
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_media_info(wav_info)
        .with_worker(worker)
        .with_prefer_hardware(backend.prefer_hardware());
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("HLS audio should open for {}: {err}", url));

    let deadline = Instant::now() + Consts::READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];
    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count > 0 => {
                return audio.position().as_secs_f64();
            }
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while warming HLS worker without seek for {url}")
            }
            Err(e) => panic!("decode error while warming HLS worker for {url}: {e}"),
        }
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
    let segment_duration =
        Consts::HLS_SEGMENT_SIZE as f64 / (Consts::HLS_SAMPLE_RATE * Consts::HLS_CHANNELS * 2.0);
    HlsTestServer::new(HlsTestServerConfig {
        custom_data: Some(Arc::new(create_wav_exact_bytes(
            signal::Sawtooth,
            44_100u32,
            2u16,
            Consts::HLS_TOTAL_BYTES,
        ))),
        segment_duration_secs: segment_duration,
        segment_size: Consts::HLS_SEGMENT_SIZE,
        segments_per_variant: Consts::HLS_SEGMENT_COUNT,
        ..Default::default()
    })
    .await
}

async fn create_packaged_single_variant_fixture(codec: AudioCodec) -> (TestServerHelper, url::Url) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(packaged_single_variant_builder(codec))
        .await
        .unwrap_or_else(|error| panic!("create packaged single-variant fixture: {error}"));
    (server, created.master_url())
}

async fn open_packaged_hls_audio(
    url: &url::Url,
    store: StoreOptions,
    _codec: AudioCodec,
    backend: DecoderBackend,
) -> Audio<Stream<Hls>> {
    let config = AudioConfig::<Hls>::new(HlsConfig::new(url.clone()).with_store(store))
        .with_prefer_hardware(backend.prefer_hardware());
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("packaged HLS audio should open for {url}: {err}"));
    audio.preload().expect("packaged HLS preload must succeed");
    audio
}

async fn read_audio_some(audio: &mut Audio<Stream<Hls>>, stage: &str) -> usize {
    let deadline = Instant::now() + Consts::READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];

    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count > 0 => return count,
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for packaged audio at stage={stage}")
            }
            Err(e) => panic!("decode error while waiting for packaged audio at stage={stage}: {e}"),
        }
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for packaged audio at stage={stage}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn read_some(resource: &mut Resource, stage: &str) -> usize {
    let deadline = Instant::now() + Consts::READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];

    loop {
        timeout(Consts::READ_TIMEOUT, resource.preload())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for preload at stage={stage}"))
            .unwrap_or_else(|err| panic!("preload failed at stage={stage}: {err}"));
        match resource.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count > 0 => return count,
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for stage={stage}")
            }
            Err(e) => panic!("decode error while waiting for stage={stage}: {e}"),
        }
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
#[case::symphonia(DecoderBackend::Symphonia)]
#[case::apple(DecoderBackend::Apple)]
#[case::android(DecoderBackend::Android)]
async fn player_resource_repeated_unavailable_mp3_does_not_panic(
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let server = TestHttpServer::new(test_app()).await;
    let store = store_options(&temp_dir, true);
    let ok_url = server.url("/ok.mp3");
    let bad_url = server.url("/gone.mp3");

    let mut ok = open_resource(&ok_url, store.clone(), backend).await;
    assert!(read_some(&mut ok, "initial_ok").await > 0);
    let forward_pos = seek_and_read(&mut ok, Duration::from_secs(2), "ok_seek_forward").await;
    assert!(
        forward_pos > 1.0,
        "forward seek should advance playback position, got {forward_pos}"
    );
    drop(ok);

    for attempt in 0..2 {
        let result = Resource::new(resource_config(
            &bad_url,
            store.clone(),
            backend,
            Some("mp3"),
            None,
        ))
        .await;
        assert!(
            result.is_err(),
            "unavailable resource attempt {attempt} must return error"
        );
    }

    let mut ok_again = open_resource(&ok_url, store, backend).await;
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
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_android(false, DecoderBackend::Android)
)]
#[case::ephemeral_symphonia(true, DecoderBackend::Symphonia)]
#[case::ephemeral_apple(true, DecoderBackend::Apple)]
#[case::ephemeral_android(true, DecoderBackend::Android)]
async fn player_resource_mp3_reopen_same_cache_keeps_backward_seek(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let server = TestHttpServer::new(test_app()).await;
    let store = store_options(&temp_dir, ephemeral);
    let ok_url = server.url("/ok.mp3");

    let mut first = open_resource(&ok_url, store.clone(), backend).await;
    assert!(read_some(&mut first, "first_initial").await > 0);
    let first_forward = seek_and_read(&mut first, Duration::from_secs(3), "first_forward").await;
    let first_backward =
        seek_and_read(&mut first, Duration::from_millis(500), "first_backward").await;
    assert!(
        first_backward < first_forward,
        "first session backward seek should move position back (forward={first_forward}, backward={first_backward})"
    );
    drop(first);

    let mut second = open_resource(&ok_url, store, backend).await;
    assert!(read_some(&mut second, "second_initial").await > 0);
    let second_forward = seek_and_read(&mut second, Duration::from_secs(3), "second_forward").await;
    let second_backward =
        seek_and_read(&mut second, Duration::from_millis(500), "second_backward").await;
    assert!(
        second_backward < second_forward,
        "reopened session backward seek should still move position back (forward={second_forward}, backward={second_backward})"
    );
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_android(false, DecoderBackend::Android)
)]
#[case::ephemeral_symphonia(true, DecoderBackend::Symphonia)]
#[case::ephemeral_apple(true, DecoderBackend::Apple)]
#[case::ephemeral_android(true, DecoderBackend::Android)]
async fn player_worker_hls_then_unavailable_mp3_then_mp3_recovery(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let hls_server = open_audio_hls_server().await;
    let file_server = TestHttpServer::new(test_app()).await;
    let player = PlayerImpl::new(PlayerConfig::default());
    let worker = player.worker().clone();
    let store = store_options(&temp_dir, ephemeral);
    let hls_url = hls_server.url("/master.m3u8");
    let ok_url = file_server.url("/ok.mp3");
    let bad_url = file_server.url("/gone.mp3");

    let hls_pos = warm_hls_worker(&hls_url, store.clone(), worker.clone(), backend).await;
    assert!(
        hls_pos > 1.0,
        "HLS warmup seek should advance playback position, got {hls_pos}"
    );

    for attempt in 0..2 {
        let result = Resource::new(resource_config_with_worker(
            &bad_url,
            store.clone(),
            worker.clone(),
            backend,
        ))
        .await;
        assert!(
            result.is_err(),
            "unavailable mp3 attempt {attempt} must return error"
        );
    }

    let mut ok = open_resource_with_worker(&ok_url, store, worker, backend).await;
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
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[case::apple(DecoderBackend::Apple)]
#[case::android(DecoderBackend::Android)]
async fn shared_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral(
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let hls_server = open_audio_hls_server().await;
    let file_server = TestHttpServer::new(test_app()).await;
    let worker = AudioWorkerHandle::new();
    let store = store_options(&temp_dir, true);
    let hls_url = hls_server.url("/master.m3u8");
    let ok_url = file_server.url("/ok.mp3");

    let hls_seek = warm_hls_worker(&hls_url, store.clone(), worker.clone(), backend).await;
    assert!(
        hls_seek > 1.0,
        "HLS warmup should advance playback position before mp3 transition, got {hls_seek}"
    );

    let mut first =
        open_resource_with_worker(&ok_url, store.clone(), worker.clone(), backend).await;
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

    let mut second = open_resource_with_worker(&ok_url, store, worker.clone(), backend).await;
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

    worker.shutdown();
}

/// How the first warmup session ends before the second begins.
#[derive(Debug, Clone, Copy)]
enum WarmupTeardown {
    /// Explicit `worker_a.shutdown()` call.
    Shutdown,
    /// Drop `worker_a` without shutdown.
    DropOnly,
    /// First session is a read-only warmup (no seek), then drop.
    ReadOnlyThenDrop,
}

/// Sequential HLS warmups from two isolated sessions must not poison each
/// other. Covers three teardown modes for the first session.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::shutdown_symphonia(WarmupTeardown::Shutdown, DecoderBackend::Symphonia)]
#[case::shutdown_apple(WarmupTeardown::Shutdown, DecoderBackend::Apple)]
#[case::shutdown_android(WarmupTeardown::Shutdown, DecoderBackend::Android)]
#[case::drop_only_symphonia(WarmupTeardown::DropOnly, DecoderBackend::Symphonia)]
#[case::drop_only_apple(WarmupTeardown::DropOnly, DecoderBackend::Apple)]
#[case::drop_only_android(WarmupTeardown::DropOnly, DecoderBackend::Android)]
#[case::read_only_symphonia(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Symphonia)]
#[case::read_only_apple(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Apple)]
#[case::read_only_android(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Android)]
async fn sequential_hls_warmup_does_not_poison_next_ephemeral_session(
    #[case] teardown: WarmupTeardown,
    #[case] backend: DecoderBackend,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let server_a = open_audio_hls_server().await;
    let server_b = open_audio_hls_server().await;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let worker_a = AudioWorkerHandle::new();
    let worker_b = AudioWorkerHandle::new();
    let store_a = store_options(&temp_a, false);
    let store_b = store_options(&temp_b, true);
    let hls_url_a = server_a.url("/master.m3u8");
    let hls_url_b = server_b.url("/master.m3u8");

    // First session: perform the requested warmup + teardown.
    let first_pos = match teardown {
        WarmupTeardown::Shutdown | WarmupTeardown::DropOnly => {
            warm_hls_worker(&hls_url_a, store_a, worker_a.clone(), backend).await
        }
        WarmupTeardown::ReadOnlyThenDrop => {
            warm_hls_worker_without_seek(&hls_url_a, store_a, worker_a.clone(), backend).await
        }
    };

    let expect_advance = !matches!(teardown, WarmupTeardown::ReadOnlyThenDrop);
    if expect_advance {
        assert!(
            first_pos > 1.0,
            "first HLS warmup must advance playback position, got {first_pos} \
             (teardown={teardown:?})",
        );
    } else {
        assert!(
            first_pos >= 0.0,
            "first HLS read-only warmup must produce samples, got {first_pos}",
        );
    }

    match teardown {
        WarmupTeardown::Shutdown => worker_a.shutdown(),
        WarmupTeardown::DropOnly | WarmupTeardown::ReadOnlyThenDrop => drop(worker_a),
    }

    let second_pos = warm_hls_worker(&hls_url_b, store_b, worker_b.clone(), backend).await;
    assert!(
        second_pos > 1.0,
        "second HLS warmup after a prior session ({teardown:?}) must still \
         advance playback position, got {second_pos}",
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
    let hls_url_a = server_a.url("/master.m3u8");
    let hls_url_b = server_b.url("/master.m3u8");

    let first_read = read_hls_stream_some(&hls_url_a, store_a).await;
    assert!(first_read > 0, "first HLS stream session must read bytes");

    let second_read = read_hls_stream_some(&hls_url_b, store_b).await;
    assert!(second_read > 0, "second HLS stream session must read bytes");
}

/// Packaged HLS audio must stay continuous in both decode and player-output paths.
///
/// This is the single-variant continuity contract for the packaged mock-server path:
/// `PcmMeta` stays contiguous, `PlaybackProgress` remains monotonic, and the offline
/// player output does not produce sustained silence or over-budget render stalls.
#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(25)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::aac_symphonia(AudioCodec::AacLc, DecoderBackend::Symphonia)]
#[case::aac_apple(AudioCodec::AacLc, DecoderBackend::Apple)]
#[case::aac_android(AudioCodec::AacLc, DecoderBackend::Android)]
#[case::flac_symphonia(AudioCodec::Flac, DecoderBackend::Symphonia)]
#[case::flac_apple(AudioCodec::Flac, DecoderBackend::Apple)]
#[case::flac_android(AudioCodec::Flac, DecoderBackend::Android)]
async fn packaged_hls_single_variant_continuity_is_stable(
    #[case] codec: AudioCodec,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    use kithara::play::internal::offline::OfflinePlayer;

    let (_server, url) = create_packaged_single_variant_fixture(codec).await;
    let store = StoreOptions::new(temp_dir.path());

    let mut progress_audio = open_packaged_hls_audio(&url, store.clone(), codec, backend).await;
    let mut progress_rx = progress_audio.events();
    let mut progress_probe = PlaybackProgressProbe::default();
    let mut total_samples = 0u64;
    let mut buf = [0.0f32; 4096];
    total_samples += read_audio_some(&mut progress_audio, "packaged_progress_warmup").await as u64;
    progress_probe.drain(&mut progress_rx);
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline && progress_probe.progress_events < 10 {
        progress_audio.preload().expect("preload must succeed");
        let read_count = match progress_audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count,
            Ok(ReadOutcome::Eof { .. }) => {
                progress_probe.drain(&mut progress_rx);
                break;
            }
            Err(e) => panic!("decode error during progress tracking: {e}"),
        };
        progress_probe.drain(&mut progress_rx);
        if read_count == 0 {
            sleep(Duration::from_millis(10)).await;
            progress_probe.observe_idle();
            continue;
        }
        total_samples += read_count as u64;
    }
    progress_probe.drain(&mut progress_rx);
    progress_probe.observe_idle();
    assert!(
        total_samples > 0,
        "{codec:?}: expected decoded output during progress tracking"
    );
    assert!(
        progress_probe.progress_events >= 4,
        "{codec:?}: expected PlaybackProgress events, got {}",
        progress_probe.progress_events
    );
    assert_eq!(
        progress_probe.regressions, 0,
        "{codec:?}: PlaybackProgress moved backward"
    );
    assert!(
        progress_probe.max_gap_between_events < Duration::from_millis(1_200),
        "{codec:?}: PlaybackProgress stalled for {:?}",
        progress_probe.max_gap_between_events
    );

    let decode_audio = open_packaged_hls_audio(&url, store, codec, backend).await;
    let mut resource = resource_from_reader(decode_audio);
    timeout(Consts::READ_TIMEOUT, resource.preload())
        .await
        .expect("packaged HLS preload must complete")
        .expect("packaged HLS preload must succeed");
    let mut player = OfflinePlayer::new(CONTINUITY_SAMPLE_RATE);
    player.load_and_fadein(resource, "packaged_single_variant");
    let _warmup = render_offline_window(
        &mut player,
        24,
        "packaged warmup",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    );
    let steady = render_offline_window(
        &mut player,
        80,
        "packaged steady-state",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    );
    assert!(
        steady.max_silence_run <= 1,
        "{codec:?}: offline output produced {} silent blocks ({steady})",
        steady.max_silence_run
    );
    assert!(
        steady.slow_renders <= 1,
        "{codec:?}: offline output exceeded render budget {} times ({steady})",
        steady.slow_renders
    );
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_android(false, DecoderBackend::Android)
)]
#[case::ephemeral_symphonia(true, DecoderBackend::Symphonia)]
#[case::ephemeral_apple(true, DecoderBackend::Apple)]
#[case::ephemeral_android(true, DecoderBackend::Android)]
async fn player_worker_hls_then_mp3_reopen_keeps_backward_seek(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let hls_server = open_audio_hls_server().await;
    let file_server = TestHttpServer::new(test_app()).await;
    let player = PlayerImpl::new(PlayerConfig::default());
    let worker = player.worker().clone();
    let store = store_options(&temp_dir, ephemeral);
    let hls_url = hls_server.url("/master.m3u8");
    let ok_url = file_server.url("/ok.mp3");

    let hls_seek = warm_hls_worker(&hls_url, store.clone(), worker.clone(), backend).await;
    assert!(
        hls_seek > 1.0,
        "HLS warmup should advance playback position before mp3 transition, got {hls_seek}"
    );

    let mut first =
        open_resource_with_worker(&ok_url, store.clone(), worker.clone(), backend).await;
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

    let mut second = open_resource_with_worker(&ok_url, store, worker, backend).await;
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
}

/// Stress test: multiple crossfade transitions on shared worker.
///
/// Tests MP3→HLS, HLS→MP3, MP3→MP3 transitions with offline render.
/// Measures per-block render time and silence gaps.
/// Every `render()` call must be complete within the audio block budget
/// (~11.6ms at 512 frames / 44100Hz), and no silence gaps > 1 block
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
    let block_budget = Duration::from_secs_f64(BLOCK as f64 / f64::from(SR));

    let hls_server = open_audio_hls_server().await;
    let store = store_options(&temp_dir(), true);
    let hls_url = hls_server.url("/master.m3u8");

    let worker = AudioWorkerHandle::new();
    let mut player = OfflinePlayer::new(SR);

    // Local MP3 path (simulates a disk-cached file, like production).
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
            let audio_config = AudioConfig::<Hls>::new(cfg)
                .with_media_info(wav_info)
                .with_worker(w);
            let audio = Audio::<Stream<Hls>>::new(audio_config)
                .await
                .expect("HLS audio");
            let mut r = resource_from_reader(audio);
            timeout(Consts::READ_TIMEOUT, r.preload())
                .await
                .expect("HLS preload")
                .expect("HLS preload result");
            r
        }
    };

    // Scenario 1: MP3 to HLS
    let mp3_1 = make_mp3(worker.clone(), store.clone()).await;
    sleep(Duration::from_millis(200)).await;
    player.load_and_fadein(mp3_1, "mp3_1");
    let s1a = render_offline_window(&mut player, 40, "MP3 solo", BLOCK, SR);

    let hls_1 = make_hls(worker.clone(), store.clone()).await;
    sleep(Duration::from_millis(200)).await;
    player.load_and_fadein(hls_1, "hls_1");
    let s1b = render_offline_window(&mut player, 80, "MP3→HLS fade", BLOCK, SR);

    // Scenario 2: HLS to MP3
    let mp3_2 = make_mp3(worker.clone(), store.clone()).await;
    sleep(Duration::from_millis(200)).await;
    player.load_and_fadein(mp3_2, "mp3_2");
    let s2 = render_offline_window(&mut player, 80, "HLS→MP3 fade", BLOCK, SR);

    // Scenario 3: MP3 to MP3
    let mp3_3 = make_mp3(worker.clone(), store.clone()).await;
    sleep(Duration::from_millis(200)).await;
    player.load_and_fadein(mp3_3, "mp3_3");
    let s3 = render_offline_window(&mut player, 80, "MP3→MP3 fade", BLOCK, SR);

    // Report
    info!("\n=== Stress crossfade results (budget={block_budget:?}) ===");
    for s in [&s1a, &s1b, &s2, &s3] {
        info!("  {s}");
    }

    // Scenario 4: repeated HLS to MP3 (intermittent glitch)
    // User reports: "чаще всего при переключении с hls на mp3,
    // это длится около 2х секунд". Run multiple iterations to catch it.
    info!("\n=== Repeated HLS→MP3 crossfade (5 iterations) ===");
    let mut worst_silence = 0u32;
    let mut worst_slow = 0u32;
    let mut worst_render = Duration::ZERO;

    for iter in 0..5 {
        // HLS phase
        let hls_n = make_hls(worker.clone(), store.clone()).await;
        sleep(Duration::from_millis(200)).await;
        player.load_and_fadein(hls_n, &format!("hls_iter{iter}"));
        let _sh = render_offline_window(&mut player, 40, &format!("HLS solo #{iter}"), BLOCK, SR);

        // Crossfade HLS→MP3
        let mp3_n = make_mp3(worker.clone(), store.clone()).await;
        sleep(Duration::from_millis(200)).await;
        player.load_and_fadein(mp3_n, &format!("mp3_iter{iter}"));
        let sm = render_offline_window(&mut player, 60, &format!("HLS→MP3 #{iter}"), BLOCK, SR);

        info!("  {sm}");
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

    info!(
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
            f64::from(s.max_silence_run) * BLOCK as f64 / f64::from(SR) * 1000.0,
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

/// MP3 through `ResourceConfig` (same path as kithara-app) must probe, decode,
/// and report correct duration — with and without extension/hint.
#[kithara::test(
    tokio,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::with_extension_symphonia("/ok.mp3", DecoderBackend::Symphonia)]
#[case::with_extension_apple("/ok.mp3", DecoderBackend::Apple)]
#[case::with_extension_android("/ok.mp3", DecoderBackend::Android)]
#[case::no_extension_symphonia("/track/stream", DecoderBackend::Symphonia)]
#[case::no_extension_apple("/track/stream", DecoderBackend::Apple)]
#[case::no_extension_android("/track/stream", DecoderBackend::Android)]
async fn resource_mp3_no_hint_decodes_with_duration(
    #[case] path: &str,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let server = TestHttpServer::new(test_app()).await;
    let url = server.url(path);
    let store = store_options(&temp_dir, true);

    // No hint — ResourceConfig must extract it from URL or pipeline must probe raw bytes.
    let config = resource_config_no_hint(&url, store, backend);
    let mut resource = Resource::new(config)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed for path={path}: {e}"));

    // Duration must be close to 187s.
    let duration = resource.duration();
    assert!(
        duration.is_some(),
        "path={path}: duration must be reported (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        (dur_secs - Consts::EXPECTED_DURATION_SECS).abs() < 2.0,
        "path={path}: expected ~{}s, got {dur_secs:.1}s",
        Consts::EXPECTED_DURATION_SECS
    );

    // Decode real PCM data — at least 2 seconds.
    let (samples, position) = {
        let mut total = 0usize;
        let mut buf = [0.0f32; 4096];
        let deadline = Instant::now() + Consts::READ_TIMEOUT;
        let mut saw_eof = false;
        loop {
            match resource.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    if count > 0 {
                        total += count;
                    }
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Err(e) => panic!("path={path}: decode error: {e}"),
            }
            if resource.position() >= Duration::from_secs(2) {
                break;
            }
            assert!(
                Instant::now() <= deadline,
                "path={path}: timed out waiting for PCM data"
            );
            sleep(Duration::from_millis(5)).await;
        }
        let _ = saw_eof;
        (total, resource.position())
    };

    assert!(samples > 0, "path={path}: must decode PCM samples");
    assert!(
        position >= Duration::from_secs(2),
        "path={path}: must decode at least 2s, got {position:?}"
    );
}

/// Live remote streams through `ResourceConfig` — same code path as kithara-app.
/// No hint, no extension manipulation — exactly what the app does.
///
/// Requires internet (silvercomet) and corporate VPN (zvuk).
#[kithara::test(
    tokio,
    timeout(Duration::from_secs(30)),
    env(
        KITHARA_HANG_TIMEOUT_SECS = "10",
        http_proxy = "",
        https_proxy = "",
        HTTP_PROXY = "",
        HTTPS_PROXY = ""
    )
)]
#[ignore = "requires internet; zvuk URLs require corporate VPN"]
#[case::silvercomet_mp3_symphonia(
    "https://stream.silvercomet.top/track.mp3",
    DecoderBackend::Symphonia
)]
#[case::silvercomet_mp3_apple("https://stream.silvercomet.top/track.mp3", DecoderBackend::Apple)]
#[case::silvercomet_mp3_android(
    "https://stream.silvercomet.top/track.mp3",
    DecoderBackend::Android
)]
#[case::silvercomet_hls_symphonia(
    "https://stream.silvercomet.top/hls/master.m3u8",
    DecoderBackend::Symphonia
)]
#[case::silvercomet_hls_apple(
    "https://stream.silvercomet.top/hls/master.m3u8",
    DecoderBackend::Apple
)]
#[case::silvercomet_hls_android(
    "https://stream.silvercomet.top/hls/master.m3u8",
    DecoderBackend::Android
)]
#[case::zvuk_27390231_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
    DecoderBackend::Symphonia
)]
#[case::zvuk_27390231_apple(
    "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
    DecoderBackend::Apple
)]
#[case::zvuk_27390231_android(
    "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
    DecoderBackend::Android
)]
#[case::zvuk_151585912_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
    DecoderBackend::Symphonia
)]
#[case::zvuk_151585912_apple(
    "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
    DecoderBackend::Apple
)]
#[case::zvuk_151585912_android(
    "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
    DecoderBackend::Android
)]
#[case::zvuk_125475417_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Symphonia
)]
#[case::zvuk_125475417_apple(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Apple
)]
#[case::zvuk_125475417_android(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Android
)]
async fn live_remote_resource_decodes_with_duration(
    #[case] url: &str,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let store = store_options(&temp_dir, true);
    let net = NetOptions {
        inactivity_timeout: Duration::from_secs(25),
        total_timeout: Some(Duration::from_secs(25)),
        ..NetOptions::default()
    };
    let downloader = Downloader::new(DownloaderConfig::default().with_net(net));
    let mut config = ResourceConfig::new(url)
        .expect("valid URL")
        .with_store(store)
        .with_downloader(downloader);
    config.prefer_hardware = backend.prefer_hardware();

    let mut resource = Resource::new(config)
        .await
        .unwrap_or_else(|e| panic!("{url}: Resource::new failed: {e}"));

    // Duration must be reported and > 30s for a real track.
    let duration = resource.duration();
    assert!(
        duration.is_some(),
        "{url}: duration must be reported (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        dur_secs > 30.0,
        "{url}: expected duration > 30s for a real track, got {dur_secs:.1}s"
    );

    // Decode at least 2 seconds of real PCM.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut samples = 0usize;
    let mut buf = [0.0f32; 4096];
    loop {
        match resource.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                if count > 0 {
                    samples += count;
                }
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("{url}: decode error: {e}"),
        }
        if resource.position() >= Duration::from_secs(2) {
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "{url}: timed out waiting for PCM data (pos={:?}, samples={samples})",
            resource.position()
        );
        sleep(Duration::from_millis(5)).await;
    }

    assert!(samples > 0, "{url}: must decode PCM samples");
    assert!(
        resource.position() >= Duration::from_secs(2),
        "{url}: must decode at least 2s, got {:?}",
        resource.position()
    );
}

/// Reproduces EXACTLY the app flow: `PlayerImpl` + `prepare_config` + `Resource::new` +
/// `select_item` + `duration_seconds()`. This is what the GUI reads.
#[kithara::test(
    tokio,
    timeout(Duration::from_secs(30)),
    env(
        KITHARA_HANG_TIMEOUT_SECS = "10",
        http_proxy = "",
        https_proxy = "",
        HTTP_PROXY = "",
        HTTPS_PROXY = ""
    )
)]
#[ignore = "requires internet; zvuk URLs require corporate VPN"]
#[case::silvercomet_mp3_symphonia(
    "https://stream.silvercomet.top/track.mp3",
    DecoderBackend::Symphonia
)]
#[case::silvercomet_mp3_apple("https://stream.silvercomet.top/track.mp3", DecoderBackend::Apple)]
#[case::silvercomet_mp3_android(
    "https://stream.silvercomet.top/track.mp3",
    DecoderBackend::Android
)]
#[case::zvuk_125475417_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Symphonia
)]
#[case::zvuk_125475417_apple(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Apple
)]
#[case::zvuk_125475417_android(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Android
)]
async fn player_mp3_duration_matches_app_flow(
    #[case] url: &str,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let store = store_options(&temp_dir, true);

    let player = PlayerImpl::new(PlayerConfig::default());
    player.reserve_slots(1);

    let mut config = ResourceConfig::new(url).unwrap().with_store(store);
    config.prefer_hardware = backend.prefer_hardware();
    player.prepare_config(&mut config);

    let resource = Resource::new(config)
        .await
        .unwrap_or_else(|e| panic!("{url}: Resource::new failed: {e}"));

    player.replace_item(0, resource);
    let _ = player.select_item(0, true);

    sleep(Duration::from_millis(500)).await;

    let dur = player.duration_seconds();
    debug!("{url} duration_seconds={dur:?}");
    assert!(
        dur.is_some(),
        "{url}: player.duration_seconds() must not be None"
    );
    let dur_secs = dur.expect("checked");
    assert!(dur_secs > 30.0, "{url}: expected >30s, got {dur_secs:.1}s");

    player.worker().shutdown();
}

/// Local fixture (`assets/track.mp3` ~162s, packaged AAC HLS ~64s) through
/// `ResourceConfig` — same code path as kithara-app. Mirrors
/// `live_remote_resource_decodes_with_duration` but against `TestServerHelper`
/// so it stays in the regular suite (no `#[ignore]`, no VPN, no internet).
#[derive(Clone, Copy, Debug)]
enum LocalKind {
    Mp3,
    HlsAac,
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
#[case::mp3_symphonia(LocalKind::Mp3, DecoderBackend::Symphonia)]
#[case::mp3_apple(LocalKind::Mp3, DecoderBackend::Apple)]
#[case::mp3_android(LocalKind::Mp3, DecoderBackend::Android)]
#[case::hls_aac_symphonia(LocalKind::HlsAac, DecoderBackend::Symphonia)]
#[case::hls_aac_apple(LocalKind::HlsAac, DecoderBackend::Apple)]
#[case::hls_aac_android(LocalKind::HlsAac, DecoderBackend::Android)]
async fn local_resource_decodes_with_duration(
    #[case] kind: LocalKind,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    let helper = TestServerHelper::new().await;
    let url = match kind {
        LocalKind::Mp3 => helper.asset("track.mp3"),
        LocalKind::HlsAac => {
            let builder = HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(16)
                .segment_duration_secs(4.0)
                .packaged_audio_aac_lc(44_100, 2);
            helper
                .create_hls(builder)
                .await
                .expect("create local HLS fixture")
                .master_url()
        }
    };
    let store = store_options(&temp_dir, true);
    let mut config = ResourceConfig::new(url.as_str())
        .expect("valid URL")
        .with_store(store);
    config.prefer_hardware = backend.prefer_hardware();

    let mut resource = Resource::new(config)
        .await
        .unwrap_or_else(|e| panic!("{url}: Resource::new failed: {e}"));

    let duration = resource.duration();
    assert!(duration.is_some(), "{url}: duration must be reported");
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        dur_secs > 30.0,
        "{url}: expected duration > 30s, got {dur_secs:.1}s"
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut samples = 0usize;
    let mut buf = [0.0f32; 4096];
    loop {
        match resource.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                if count > 0 {
                    samples += count;
                }
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("{url}: decode error: {e}"),
        }
        if resource.position() >= Duration::from_secs(2) {
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "{url}: timed out waiting for PCM (pos={:?}, samples={samples})",
            resource.position()
        );
        sleep(Duration::from_millis(5)).await;
    }

    assert!(samples > 0, "{url}: must decode PCM samples");
    assert!(
        resource.position() >= Duration::from_secs(2),
        "{url}: must decode at least 2s, got {:?}",
        resource.position()
    );
}

// Note: the remote-only sibling `player_mp3_duration_matches_app_flow` exercises
// `PlayerImpl::duration_seconds()`, which is updated by the running processor
// (`player_processor::update_position_duration`). That path needs `play()` and
// therefore a real cpal device — it can't be reproduced offline without
// audio hardware, so we don't add a local equivalent here.
