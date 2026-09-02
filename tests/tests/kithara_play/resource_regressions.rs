#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{io::Read, num::NonZeroUsize};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ConsumerWakeMode, ReadOutcome},
    decode::DecoderBackend,
    file::{File as FileSource, FileConfig, FileSrc},
    hls::{Hls, HlsConfig},
    platform::{
        CancelScope, CancelToken,
        sync::Arc,
        time::{Duration, Instant, sleep, timeout},
    },
    play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, RegisteredAudio, Resource,
        ResourceConfig, ResourceSrc,
    },
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::PackagedSignal,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    offline::{OfflineSession, resource_from_reader},
    temp_dir,
};
use kithara_test_fixtures::{
    SignalAsset,
    assets::signal_mp3_track_sine440_187s,
    signal::{self, Wave},
};
use tracing::info;

use crate::{
    bufpool_ext::{Pools, TestPools, pools},
    common::test_defaults::Consts as Shared,
    continuity::{
        CONTINUITY_BLOCK_FRAMES, CONTINUITY_SAMPLE_RATE, PlaybackProgressProbe,
        render_offline_window,
    },
};

struct Consts;
impl Consts {
    const READ_TIMEOUT: Duration = Shared::READ_TIMEOUT;
    const HLS_SEGMENT_COUNT: usize = 3;
    const HLS_SEGMENT_SIZE: usize = Shared::SEGMENT_SIZE;
    const HLS_TOTAL_BYTES: usize = Self::HLS_SEGMENT_COUNT * Self::HLS_SEGMENT_SIZE;
    const HLS_SAMPLE_RATE: f64 = Shared::SAMPLE_RATE as f64;
    const HLS_CHANNELS: f64 = Shared::CHANNELS as f64;
    /// Expected duration of the generated `signal_mp3_track_sine440_187s` clip.
    const EXPECTED_DURATION_SECS: f64 = Shared::TEST_MP3_DURATION_SECS;
}

fn play_worker(pools: &Pools) -> PlayWorker<TestPools> {
    PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build())
}

fn play_worker_with_cancel(pools: &Pools, cancel: CancelToken) -> PlayWorker<TestPools> {
    PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel)
            .build(),
    )
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

/// (ok mp3 url with a `.mp3` extension, unavailable 503 url) on the shared server.
async fn mp3_endpoints() -> (url::Url, url::Url) {
    let helper = TestServerHelper::new().await;
    let ok = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let gone = helper.register_behavior(FixtureBehavior {
        content: Content::Status(503),
        delivery: Delivery::Normal,
    });
    (ok.child_url("ok.mp3"), gone.url())
}

fn asset_store(temp_dir: &TestTempDir, ephemeral: bool, pools: &Pools) -> AssetStore<TestPools> {
    if ephemeral {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cache_capacity(NonZeroUsize::new(4).expect("nonzero"))
            .max_assets(8)
            .build()
    } else {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: temp_dir.path().to_path_buf(),
            })
            .build()
    }
}

/// Build a `ResourceConfig` with the common shape used throughout this
/// file: backend-preferred hardware flag, optional MP3 hint, optional
/// shared audio worker handle.
fn resource_config(
    url: &url::Url,
    store: AssetStore<TestPools>,
    backend: DecoderBackend,
    hint: Option<&str>,
    worker: PlayWorker<TestPools>,
) -> ResourceConfig<TestPools> {
    ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).unwrap())
        .store(store)
        .maybe_hint(hint)
        .worker(worker)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .build()
}

/// Open a resource with [`resource_config`] options; panics on error.
async fn open_resource_full(
    url: &url::Url,
    store: AssetStore<TestPools>,
    backend: DecoderBackend,
    hint: Option<&str>,
    worker: PlayWorker<TestPools>,
) -> Resource {
    Resource::new(resource_config(url, store, backend, hint, worker))
        .await
        .unwrap_or_else(|err| panic!("resource should open for {}: {err}", url))
}

async fn open_resource(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
) -> Resource {
    open_resource_full(url, store, backend, Some("mp3"), worker).await
}

async fn open_resource_with_worker(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
) -> Resource {
    open_resource_full(url, store, backend, Some("mp3"), worker).await
}

fn resource_config_with_worker(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
) -> ResourceConfig<TestPools> {
    resource_config(url, store, backend, Some("mp3"), worker)
}

fn resource_config_no_hint(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
) -> ResourceConfig<TestPools> {
    resource_config(url, store, backend, None, worker)
}

// Keep this warmup nonblocking: under full-suite load a blocking underrun arms
// the consumer hang watchdog before the HLS producer necessarily gets scheduled.
// The loop already drives preload and handles Pending explicitly.
#[kithara::flash(true)]
async fn warm_hls_worker(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
) -> f64 {
    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let hls_config = HlsConfig::for_url(url.clone())
        .store(store)
        .pools(worker.pools().clone())
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .build();
    let mut audio = worker
        .open(config)
        .await
        .unwrap_or_else(|err| panic!("HLS audio should open for {}: {err}", url));

    let mut buf = [0.0f32; 4096];
    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count.get() > 0 => break,
            Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while warming HLS worker for {url}")
            }
            Err(e) => panic!("decode error while warming HLS worker for {url}: {e}"),
        }
        sleep(Duration::from_millis(10)).await;
    }

    audio
        .seek(Duration::from_secs(2))
        .unwrap_or_else(|err| panic!("HLS warmup seek must succeed for {}: {err}", url));

    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count.get() > 0 => {
                return audio.position().as_secs_f64();
            }
            Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF after HLS warmup seek for {url}")
            }
            Err(e) => panic!("decode error after HLS warmup seek for {url}: {e}"),
        }
        sleep(Duration::from_millis(10)).await;
    }
}

// Same nonblocking warmup contract as `warm_hls_worker`.
#[kithara::flash(true)]
async fn warm_hls_worker_without_seek(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
) -> f64 {
    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let hls_config = HlsConfig::for_url(url.clone())
        .store(store)
        .pools(worker.pools().clone())
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .build();
    let mut audio = worker
        .open(config)
        .await
        .unwrap_or_else(|err| panic!("HLS audio should open for {}: {err}", url));

    let mut buf = [0.0f32; 4096];
    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count.get() > 0 => {
                return audio.position().as_secs_f64();
            }
            Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while warming HLS worker without seek for {url}")
            }
            Err(e) => panic!("decode error while warming HLS worker for {url}: {e}"),
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn read_hls_stream_some(
    url: &url::Url,
    store: AssetStore<TestPools>,
    pools: &Pools,
) -> usize {
    let config = HlsConfig::for_url(url.clone())
        .store(store)
        .pools(pools.clone())
        .build();
    let mut stream = Stream::<Hls<TestPools>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("HLS stream should open for {}: {err}", url));
    let mut buf = [0_u8; 4096];
    read_hls_stream_bytes(&mut stream, &mut buf, url)
}

/// `no_block`: the synchronous HLS stream read crosses the platform gate that this regression exercises.
#[kithara::allow_block]
fn read_hls_stream_bytes(
    stream: &mut Stream<Hls<TestPools>>,
    buf: &mut [u8],
    url: &url::Url,
) -> usize {
    stream
        .read(buf)
        .unwrap_or_else(|err| panic!("HLS stream should read for {}: {err}", url))
}

async fn open_audio_hls_server() -> HlsTestServer {
    let segment_duration =
        Consts::HLS_SEGMENT_SIZE as f64 / (Consts::HLS_SAMPLE_RATE * Consts::HLS_CHANNELS * 2.0);
    HlsTestServer::new(HlsTestServerConfig {
        custom_data: Some(Arc::new(signal::wav_of_size(
            44_100u32,
            2u16,
            Consts::HLS_TOTAL_BYTES,
            Wave::Sawtooth,
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
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    _codec: AudioCodec,
    backend: DecoderBackend,
    wake_mode: ConsumerWakeMode,
) -> RegisteredAudio<Stream<Hls<TestPools>>, TestPools> {
    let hls = HlsConfig::for_url(url.clone())
        .store(store)
        .pools(worker.pools().clone())
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .consumer_wake_mode(wake_mode)
        .build();
    let mut audio = worker
        .open(config)
        .await
        .unwrap_or_else(|err| panic!("packaged HLS audio should open for {url}: {err}"));
    audio.preload().expect("packaged HLS preload must succeed");
    audio
}

async fn read_audio_some(
    audio: &mut RegisteredAudio<Stream<Hls<TestPools>>, TestPools>,
    stage: &str,
) -> usize {
    let deadline = Instant::now() + Consts::READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];

    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count.get() > 0 => return count.get(),
            Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {}
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
            Ok(ReadOutcome::Frames { count, .. }) if count.get() > 0 => return count.get(),
            Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                // The duration decides where a seek lands: the seek engine
                // reports EOF outright when the target is at or past it. A
                // wrong duration therefore produces this EOF without any
                // decode, and the startup probe that measures it gives up on
                // the first byte range that is not ready yet — so print what
                // was measured alongside where we are.
                panic!(
                    "unexpected EOF while waiting for stage={stage}                      (duration={:?}, position={:?})",
                    resource.duration(),
                    Resource::position(resource),
                )
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

#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn player_resource_repeated_unavailable_mp3_does_not_panic(
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (ok_url, bad_url) = mp3_endpoints().await;
    let region = pools();
    let store = asset_store(&temp_dir, true, &region);

    let mut ok = open_resource(&ok_url, store.clone(), play_worker(&region), backend).await;
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
            play_worker(&region),
        ))
        .await;
        assert!(
            result.is_err(),
            "unavailable resource attempt {attempt} must return error"
        );
    }

    let mut ok_again = open_resource(&ok_url, store, play_worker(&region), backend).await;
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

#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::disk_android(false, DecoderBackend::Android)
)]
#[case::ephemeral_symphonia(true, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::ephemeral_apple(true, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::ephemeral_android(true, DecoderBackend::Android)
)]
async fn player_resource_mp3_reopen_same_cache_keeps_backward_seek(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (ok_url, _) = mp3_endpoints().await;
    let region = pools();
    let store = asset_store(&temp_dir, ephemeral, &region);

    let mut first = open_resource(&ok_url, store.clone(), play_worker(&region), backend).await;
    assert!(read_some(&mut first, "first_initial").await > 0);
    let first_forward = seek_and_read(&mut first, Duration::from_secs(3), "first_forward").await;
    let first_backward =
        seek_and_read(&mut first, Duration::from_millis(500), "first_backward").await;
    assert!(
        first_backward < first_forward,
        "first session backward seek should move position back (forward={first_forward}, backward={first_backward})"
    );
    drop(first);

    let mut second = open_resource(&ok_url, store, play_worker(&region), backend).await;
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
    flash(false),
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(5)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::disk_android(false, DecoderBackend::Android)
)]
#[case::ephemeral_symphonia(true, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::ephemeral_apple(true, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::ephemeral_android(true, DecoderBackend::Android)
)]
async fn player_worker_hls_then_unavailable_mp3_then_mp3_recovery(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let hls_server = open_audio_hls_server().await;
    let (ok_url, bad_url) = mp3_endpoints().await;
    let region = pools();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .worker(play_worker(&region))
            .session(OfflineSession::arc_manual())
            .build(),
    );
    let worker = player.worker().clone();
    let store = asset_store(&temp_dir, ephemeral, &region);
    let hls_url = hls_server.url("/master.m3u8");

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

#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn shared_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral(
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let hls_server = open_audio_hls_server().await;
    let (ok_url, _) = mp3_endpoints().await;
    let region = pools();
    let worker = play_worker(&region);
    let store = asset_store(&temp_dir, true, &region);
    let hls_url = hls_server.url("/master.m3u8");

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

    drop(second);
    drop(worker);
}

/// How the first warmup session ends before the second begins.
#[derive(Debug, Clone, Copy)]
enum WarmupTeardown {
    /// Explicit cancellation of the worker's parent token.
    Shutdown,
    /// Drop `worker_a` without shutdown.
    DropOnly,
    /// First session is a read-only warmup (no seek), then drop.
    ReadOnlyThenDrop,
}

/// Sequential HLS warmups from two isolated sessions must not poison each
/// other. Covers three teardown modes for the first session.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[case::shutdown_symphonia(WarmupTeardown::Shutdown, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::shutdown_apple(WarmupTeardown::Shutdown, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::shutdown_android(WarmupTeardown::Shutdown, DecoderBackend::Android)
)]
#[case::drop_only_symphonia(WarmupTeardown::DropOnly, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drop_only_apple(WarmupTeardown::DropOnly, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::drop_only_android(WarmupTeardown::DropOnly, DecoderBackend::Android)
)]
#[case::read_only_symphonia(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::read_only_apple(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::read_only_android(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Android)
)]
async fn sequential_hls_warmup_does_not_poison_next_ephemeral_session(
    #[case] teardown: WarmupTeardown,
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let server_a = open_audio_hls_server().await;
    let server_b = open_audio_hls_server().await;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let region_a = pools();
    let region_b = pools();
    let worker_a_scope = CancelScope::new(None);
    let worker_a = play_worker_with_cancel(&region_a, worker_a_scope.token());
    let worker_b = play_worker(&region_b);
    let store_a = asset_store(&temp_a, false, &region_a);
    let store_b = asset_store(&temp_b, true, &region_b);
    let hls_url_a = server_a.url("/master.m3u8");
    let hls_url_b = server_b.url("/master.m3u8");

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
        WarmupTeardown::Shutdown => {
            worker_a_scope.cancel();
            drop(worker_a);
        }
        WarmupTeardown::DropOnly | WarmupTeardown::ReadOnlyThenDrop => drop(worker_a),
    }

    let second_pos = warm_hls_worker(&hls_url_b, store_b, worker_b.clone(), backend).await;
    assert!(
        second_pos > 1.0,
        "second HLS warmup after a prior session ({teardown:?}) must still \
         advance playback position, got {second_pos}",
    );
    drop(worker_b);
}

#[kithara::test(
    tokio,
    multi_thread,
    browser,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(5)
)]
async fn sequential_hls_stream_sessions_do_not_poison_next_ephemeral_session() {
    let server_a = open_audio_hls_server().await;
    let server_b = open_audio_hls_server().await;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let pools_a = pools();
    let pools_b = pools();
    let store_a = asset_store(&temp_a, false, &pools_a);
    let store_b = asset_store(&temp_b, true, &pools_b);
    let hls_url_a = server_a.url("/master.m3u8");
    let hls_url_b = server_b.url("/master.m3u8");

    let first_read = read_hls_stream_some(&hls_url_a, store_a, &pools_a).await;
    assert!(first_read > 0, "first HLS stream session must read bytes");

    let second_read = read_hls_stream_some(&hls_url_b, store_b, &pools_b).await;
    assert!(second_read > 0, "second HLS stream session must read bytes");
}

#[kithara::test(tokio, native, timeout(Duration::from_secs(25)), hang_timeout_secs(3))]
#[case::aac_symphonia(AudioCodec::AacLc, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_apple(AudioCodec::AacLc, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::aac_android(AudioCodec::AacLc, DecoderBackend::Android)
)]
#[case::flac_symphonia(AudioCodec::Flac, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple(AudioCodec::Flac, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::flac_android(AudioCodec::Flac, DecoderBackend::Android)
)]
async fn packaged_hls_single_variant_continuity_is_stable(
    #[case] codec: AudioCodec,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    use kithara_integration_tests::offline::OfflinePlayer;

    let (_server, url) = create_packaged_single_variant_fixture(codec).await;
    let region = pools();
    let store = asset_store(&temp_dir, false, &region);

    let mut progress_audio = open_packaged_hls_audio(
        &url,
        store.clone(),
        play_worker(&region),
        codec,
        backend,
        ConsumerWakeMode::ImmediateOffRt,
    )
    .await;
    let mut progress_rx = progress_audio.event_bus().subscribe();
    let mut progress_probe = PlaybackProgressProbe::default();
    let mut total_samples = 0u64;
    let mut buf = [0.0f32; 4096];
    total_samples += read_audio_some(&mut progress_audio, "packaged_progress_warmup").await as u64;
    progress_probe.drain(&mut progress_rx);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(4);
    let mut frame_reads = 0u64;
    let mut pending_reads = 0u64;
    let mut saw_eof = false;
    while Instant::now() < deadline && progress_probe.progress_events < 10 {
        progress_audio.preload().expect("preload must succeed");
        let read_count = match progress_audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            Ok(ReadOutcome::Pending { .. }) => 0,
            Ok(ReadOutcome::Eof { .. }) => {
                saw_eof = true;
                progress_probe.drain(&mut progress_rx);
                break;
            }
            Err(e) => panic!("decode error during progress tracking: {e}"),
        };
        progress_probe.drain(&mut progress_rx);
        if read_count == 0 {
            pending_reads += 1;
            time::sleep(Duration::from_millis(10)).await;
            progress_probe.observe_idle();
            continue;
        }
        frame_reads += 1;
        total_samples += read_count as u64;
    }
    progress_probe.drain(&mut progress_rx);
    progress_probe.observe_idle();
    let elapsed = started.elapsed();
    assert!(
        total_samples > 0,
        "{codec:?}: expected decoded output during progress tracking; \
         frame_reads={frame_reads}, pending_reads={pending_reads}, saw_eof={saw_eof}, \
         elapsed={elapsed:?}"
    );
    assert!(
        progress_probe.progress_events >= 4,
        "{codec:?}: expected PlaybackProgress events, got {}; total_samples={total_samples}, \
         frame_reads={frame_reads}, pending_reads={pending_reads}, saw_eof={saw_eof}, \
         elapsed={elapsed:?}",
        progress_probe.progress_events
    );
    assert_eq!(
        progress_probe.regressions, 0,
        "{codec:?}: PlaybackProgress moved backward"
    );
    assert!(
        progress_probe.max_gap_between_events < Duration::from_millis(1_200),
        "{codec:?}: PlaybackProgress stalled for {:?}; total_samples={total_samples}, \
         frame_reads={frame_reads}, pending_reads={pending_reads}, saw_eof={saw_eof}, \
         progress_events={}, elapsed={elapsed:?}",
        progress_probe.max_gap_between_events,
        progress_probe.progress_events
    );

    let decode_audio = open_packaged_hls_audio(
        &url,
        store,
        play_worker(&region),
        codec,
        backend,
        ConsumerWakeMode::RealtimeDeferred,
    )
    .await;
    let mut resource = resource_from_reader(decode_audio);
    time::timeout(Consts::READ_TIMEOUT, resource.preload())
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

#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::disk_android(false, DecoderBackend::Android)
)]
#[case::ephemeral_symphonia(true, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::ephemeral_apple(true, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::ephemeral_android(true, DecoderBackend::Android)
)]
async fn player_worker_hls_then_mp3_reopen_keeps_backward_seek(
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let hls_server = open_audio_hls_server().await;
    let (ok_url, _) = mp3_endpoints().await;
    let region = pools();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .worker(play_worker(&region))
            .session(OfflineSession::arc_manual())
            .build(),
    );
    let worker = player.worker().clone();
    let store = asset_store(&temp_dir, ephemeral, &region);
    let hls_url = hls_server.url("/master.m3u8");

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
    hang_timeout_secs(10),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_play=debug,kithara_stream=debug")
)]
async fn stress_offline_crossfade_no_gaps() {
    use kithara_integration_tests::offline::OfflinePlayer;

    const BLOCK: usize = 512;
    const SR: u32 = 44100;
    let block_budget = Duration::from_secs_f64(BLOCK as f64 / f64::from(SR));

    let hls_server = open_audio_hls_server().await;
    let region = pools();
    let store = asset_store(&temp_dir(), true, &region);
    let hls_url = hls_server.url("/master.m3u8");

    let master_scope = CancelScope::new(None);
    let master_cancel = master_scope.token();
    let worker = play_worker_with_cancel(&region, master_cancel.child());
    let mut player = OfflinePlayer::new(SR);

    let media_dir = temp_dir();
    let local_mp3 = media_dir.write("track.mp3", signal_mp3_track_sine440_187s().bytes());

    let make_mp3 = |w: PlayWorker<TestPools>, s: AssetStore<TestPools>, cancel: CancelToken| {
        let p = local_mp3.clone();
        async move {
            let pools = w.pools().clone();
            let file_cfg = FileConfig::for_src(FileSrc::Local(p))
                .store(s)
                .pools(pools)
                .cancel(cancel.clone())
                .build();
            let audio_cfg = AudioConfig::<FileSource<TestPools>>::for_stream(file_cfg)
                .hint("mp3".to_string())
                .cancel(cancel)
                .build();
            let audio = w.open(audio_cfg).await.expect("create local MP3 audio");
            resource_from_reader(audio)
        }
    };

    let make_hls = |w: PlayWorker<TestPools>, s: AssetStore<TestPools>, cancel: CancelToken| {
        let u = hls_url.clone();
        async move {
            let pools = w.pools().clone();
            let wav_info = MediaInfo::builder()
                .maybe_codec(Some(AudioCodec::Pcm))
                .maybe_container(Some(ContainerFormat::Wav))
                .build();
            let cfg = HlsConfig::for_url(u)
                .store(s)
                .pools(pools)
                .cancel(cancel.clone())
                .build();
            let audio_config = AudioConfig::<Hls<TestPools>>::for_stream(cfg)
                .media_info(wav_info)
                .cancel(cancel)
                .build();
            let audio = w.open(audio_config).await.expect("HLS audio");
            let mut r = resource_from_reader(audio);
            time::timeout(Consts::READ_TIMEOUT, r.preload())
                .await
                .expect("HLS preload")
                .expect("HLS preload result");
            r
        }
    };

    let mut mp3_1 = make_mp3(worker.clone(), store.clone(), master_cancel.child()).await;
    time::timeout(Consts::READ_TIMEOUT, mp3_1.preload())
        .await
        .expect("mp3_1 preload deadline")
        .expect("mp3_1 preload");
    player.load_and_fadein(mp3_1, "mp3_1");
    let s1a = render_offline_window(&mut player, 40, "MP3 solo", BLOCK, SR);

    let mut hls_1 = make_hls(worker.clone(), store.clone(), master_cancel.child()).await;
    time::timeout(Consts::READ_TIMEOUT, hls_1.preload())
        .await
        .expect("hls_1 preload deadline")
        .expect("hls_1 preload");
    player.load_and_fadein(hls_1, "hls_1");
    let s1b = render_offline_window(&mut player, 80, "MP3→HLS fade", BLOCK, SR);

    let mut mp3_2 = make_mp3(worker.clone(), store.clone(), master_cancel.child()).await;
    time::timeout(Consts::READ_TIMEOUT, mp3_2.preload())
        .await
        .expect("mp3_2 preload deadline")
        .expect("mp3_2 preload");
    player.load_and_fadein(mp3_2, "mp3_2");
    let s2 = render_offline_window(&mut player, 80, "HLS→MP3 fade", BLOCK, SR);

    let mut mp3_3 = make_mp3(worker.clone(), store.clone(), master_cancel.child()).await;
    time::timeout(Consts::READ_TIMEOUT, mp3_3.preload())
        .await
        .expect("mp3_3 preload deadline")
        .expect("mp3_3 preload");
    player.load_and_fadein(mp3_3, "mp3_3");
    let s3 = render_offline_window(&mut player, 80, "MP3→MP3 fade", BLOCK, SR);

    info!("\n=== Stress crossfade results (budget={block_budget:?}) ===");
    for s in [&s1a, &s1b, &s2, &s3] {
        info!("  {s}");
    }

    info!("\n=== Repeated HLS→MP3 crossfade (5 iterations) ===");
    let mut worst_silence = 0u32;
    let mut worst_slow = 0u32;
    let mut worst_render = Duration::ZERO;

    for iter in 0..5 {
        let mut hls_n = make_hls(worker.clone(), store.clone(), master_cancel.child()).await;
        time::timeout(Consts::READ_TIMEOUT, hls_n.preload())
            .await
            .expect("hls_n preload deadline")
            .expect("hls_n preload");
        player.load_and_fadein(hls_n, &format!("hls_iter{iter}"));
        let _sh = render_offline_window(&mut player, 40, &format!("HLS solo #{iter}"), BLOCK, SR);

        let mut mp3_n = make_mp3(worker.clone(), store.clone(), master_cancel.child()).await;
        time::timeout(Consts::READ_TIMEOUT, mp3_n.preload())
            .await
            .expect("mp3_n preload deadline")
            .expect("mp3_n preload");
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

    master_scope.cancel();
    drop(worker);

    info!(
        "\n  Worst across 5 HLS→MP3: silence={worst_silence} slow={worst_slow} \
         max_render={worst_render:?}"
    );

    let all = [&s1b, &s2, &s3];
    for s in &all {
        assert!(
            s.max_silence_run <= 2,
            "{}: silence gap {} blocks ({:.1}ms) — audio underrun during crossfade",
            s.label,
            s.max_silence_run,
            f64::from(s.max_silence_run) * BLOCK as f64 / f64::from(SR) * 1000.0,
        );
        // Wall-clock render budget. RTSan instruments every malloc/lock in
        // the whole process, inflating render wall-clock far past the audio
        // block budget; it cannot judge this throughput contract. RTSan still
        // runs the crossfade path to detect real RT violations in `process()`.
        #[cfg(not(rtsan))]
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
    #[cfg(not(rtsan))]
    assert!(
        worst_slow <= 1,
        "HLS→MP3 repeated: {worst_slow} blocks exceeded budget, \
         max_render={worst_render:?} — sustained blocking during crossfade"
    );
}

/// MP3 through `ResourceConfig` (same path as kithara-app) must probe, decode,
/// and report correct duration — with and without extension/hint.
#[kithara::test(tokio, timeout(Duration::from_secs(15)), hang_timeout_secs(5))]
#[case::with_extension_symphonia(Some("track.mp3"), DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::with_extension_apple(Some("track.mp3"), DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::with_extension_android(Some("track.mp3"), DecoderBackend::Android)
)]
#[case::no_extension_symphonia(None, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::no_extension_apple(None, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::no_extension_android(None, DecoderBackend::Android)
)]
async fn resource_mp3_no_hint_decodes_with_duration(
    #[case] suffix: Option<&str>,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let url = match suffix {
        Some(s) => handle.child_url(s),
        None => handle.url(),
    };
    let region = pools();
    let store = asset_store(&temp_dir, true, &region);
    let path = url.as_str();

    let config = resource_config_no_hint(&url, store, play_worker(&region), backend);
    let mut resource = Resource::new(config)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed for path={path}: {e}"));

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

    let (samples, position) = {
        let mut total = 0usize;
        let mut buf = [0.0f32; 4096];
        let deadline = Instant::now() + Consts::READ_TIMEOUT;
        let mut saw_eof = false;
        loop {
            match resource.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    let count = count.get();
                    if count > 0 {
                        total += count;
                    }
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Ok(ReadOutcome::Pending { .. }) => {}
                Err(e) => panic!("path={path}: decode error: {e}"),
            }
            if resource.position() >= Duration::from_secs(2) {
                break;
            }
            assert!(
                Instant::now() <= deadline,
                "path={path}: timed out waiting for PCM data"
            );
            time::sleep(Duration::from_millis(5)).await;
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

/// Local fixture (the generated ~187s MPEG clip, packaged AAC HLS ~64s) through
/// `ResourceConfig` — same code path as kithara-app. Mirrors
/// `live_remote_resource_decodes_with_duration` (now in
/// `live_remote_network.rs`) but against `TestServerHelper`, so it stays in the
/// regular suite: no VPN, no internet.
#[derive(Clone, Copy, Debug)]
enum LocalKind {
    Mp3,
    HlsAac,
}

#[kithara::test(tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(10))]
#[case::mp3_symphonia(LocalKind::Mp3, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_apple(LocalKind::Mp3, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::mp3_android(LocalKind::Mp3, DecoderBackend::Android)
)]
#[case::hls_aac_symphonia(LocalKind::HlsAac, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_aac_apple(LocalKind::HlsAac, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::hls_aac_android(LocalKind::HlsAac, DecoderBackend::Android)
)]
async fn local_resource_decodes_with_duration(
    #[case] kind: LocalKind,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    let helper = TestServerHelper::new().await;
    let url = match kind {
        LocalKind::Mp3 => helper.signal(SignalAsset::MP3_SINE880_48K_162S),
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
    let region = pools();
    let store = asset_store(&temp_dir, true, &region);
    let config: ResourceConfig<TestPools> =
        ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid URL"))
            .store(store)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .worker(play_worker(&region))
            .build();

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
                let count = count.get();
                if count > 0 {
                    samples += count;
                }
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Ok(ReadOutcome::Pending { .. }) => {}
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
        time::sleep(Duration::from_millis(5)).await;
    }

    assert!(samples > 0, "{url}: must decode PCM samples");
    assert!(
        resource.position() >= Duration::from_secs(2),
        "{url}: must decode at least 2s, got {:?}",
        resource.position()
    );
}
