#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

pub(super) use std::{
    io::Read,
    num::{NonZeroU32, NonZeroUsize},
};

pub(super) use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, ReadOutcome},
    decode::DecoderBackend,
    hls::{Hls, HlsConfig},
    host::{HostConfig, HostSettings},
    platform::{
        CancelScope, CancelToken,
        sync::Arc,
        time::{Duration, WallInstant, sleep, timeout},
    },
    play::{PlayWorker, PlayWorkerConfig, Resource, ResourceConfig, ResourceSrc},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
pub(super) use kithara_integration_tests::{
    Content, CreatedHls, Delivery, FixtureBehavior, HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::PackagedSignal,
    output_continuity::{CONTINUITY_SAMPLE_RATE, render_offline_window},
};
pub(super) use kithara_test_fixtures::{
    SignalAsset, fixtures::tone_mp3, integration_fixtures::saw_segments,
};
pub(super) use kithara_test_utils::{TestTempDir, temp_dir};
pub(super) use num_traits::AsPrimitive;
pub(super) use tracing::info;

use super::consts;
pub(super) use crate::{
    bufpool_ext::{Pools, TestPools, pools},
    common::test_defaults::consts as shared,
};

pub(super) fn play_worker(pools: &Pools) -> PlayWorker<TestPools> {
    PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build())
}

pub(super) fn play_worker_with_cancel(pools: &Pools, cancel: CancelToken) -> PlayWorker<TestPools> {
    PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel)
            .build(),
    )
}

pub(super) fn packaged_single_variant_builder(codec: AudioCodec) -> HlsFixtureBuilder {
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
#[kithara::fixture]
pub(super) async fn mp3_endpoints(
    tone_mp3: &'static [u8],
) -> (TestServerHelper, url::Url, url::Url) {
    mp3_endpoints_for_bytes(tone_mp3.to_vec()).await
}

pub(super) async fn mp3_endpoints_for_bytes(
    bytes: Vec<u8>,
) -> (TestServerHelper, url::Url, url::Url) {
    let helper = TestServerHelper::new().await;
    let ok = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(bytes),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let gone = helper.register_behavior(FixtureBehavior {
        content: Content::Status(503),
        delivery: Delivery::Normal,
    });
    (helper, ok.child_url("ok.mp3"), gone.url())
}

pub(super) fn asset_store(
    temp_dir: &TestTempDir,
    ephemeral: bool,
    pools: &Pools,
) -> AssetStore<TestPools> {
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
pub(super) fn resource_config(
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

pub(super) async fn open_resource(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
) -> Resource {
    let config = resource_config(url, store, backend, Some("mp3"), worker);
    kithara_integration_tests::mock::open_resource(&config)
        .await
        .unwrap_or_else(|err| panic!("resource should open for {}: {err}", url))
}

// Keep this warmup nonblocking: under full-suite load a blocking underrun arms
// the consumer hang watchdog before the HLS producer necessarily gets scheduled.
// The loop already drives preload and handles Pending explicitly.
#[kithara::flash(true)]
pub(super) async fn warm_hls_worker(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    backend: DecoderBackend,
    seek: Option<Duration>,
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
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .unwrap_or_else(|err| panic!("HLS audio should open for {}: {err}", url));

    let mut buf = [0.0f32; 4096];
    loop {
        audio.preload().expect("preload must succeed");
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) if count.get() > 0 => {
                if seek.is_none() {
                    return audio.position().as_secs_f64();
                }
                break;
            }
            Ok(ReadOutcome::Frames { .. }) | Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while warming HLS worker for {url}")
            }
            Err(e) => panic!("decode error while warming HLS worker for {url}: {e}"),
        }
        sleep(Duration::from_millis(10)).await;
    }

    audio
        .seek(seek.expect("seek checked above"))
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

pub(super) async fn read_hls_stream_some(
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
pub(super) fn read_hls_stream_bytes(
    stream: &mut Stream<Hls<TestPools>>,
    buf: &mut [u8],
    url: &url::Url,
) -> usize {
    stream
        .read(buf)
        .unwrap_or_else(|err| panic!("HLS stream should read for {}: {err}", url))
}

#[kithara::fixture]
pub(super) async fn open_audio_hls_server(saw_segments: &'static [u8]) -> CreatedHls {
    let segment_size: f64 = consts::HLS_SEGMENT_SIZE.as_();
    let segment_duration = segment_size / (consts::HLS_SAMPLE_RATE * consts::HLS_CHANNELS * 2.0);
    TestServerHelper::new()
        .await
        .create_hls(
            HlsFixtureBuilder::new()
                .custom_data(Arc::new(saw_segments.to_vec()))
                .codecs("wav".to_string())
                .segment_duration_secs(segment_duration)
                .segment_size(consts::HLS_SEGMENT_SIZE)
                .segments_per_variant(consts::HLS_SEGMENT_COUNT),
        )
        .await
        .expect("create HLS fixture")
}

pub(super) async fn create_packaged_single_variant_fixture(
    codec: AudioCodec,
) -> (TestServerHelper, url::Url) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(packaged_single_variant_builder(codec))
        .await
        .unwrap_or_else(|error| panic!("create packaged single-variant fixture: {error}"));
    (server, created.master_url())
}

pub(super) async fn open_packaged_hls_audio(
    url: &url::Url,
    store: AssetStore<TestPools>,
    worker: PlayWorker<TestPools>,
    _codec: AudioCodec,
    backend: DecoderBackend,
) -> kithara_integration_tests::mock::LaneAudio<Stream<Hls<TestPools>>, TestPools> {
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
        .build();
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .unwrap_or_else(|err| panic!("packaged HLS audio should open for {url}: {err}"));
    audio.preload().expect("packaged HLS preload must succeed");
    audio
}

pub(super) async fn read_audio_some(
    audio: &mut kithara_integration_tests::mock::LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    stage: &str,
) -> usize {
    let deadline = WallInstant::now() + consts::READ_TIMEOUT;
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
            WallInstant::now() <= deadline,
            "timed out waiting for packaged audio at stage={stage}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

pub(super) async fn read_some(resource: &mut Resource, stage: &str) -> usize {
    let deadline = WallInstant::now() + consts::READ_TIMEOUT;
    let mut buf = [0.0f32; 4096];

    loop {
        timeout(consts::READ_TIMEOUT, resource.preload())
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
            WallInstant::now() <= deadline,
            "timed out waiting for decoded PCM at stage={stage}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

pub(super) async fn seek_and_read(resource: &mut Resource, position: Duration, stage: &str) -> f64 {
    resource
        .seek(position)
        .unwrap_or_else(|err| panic!("seek must succeed at stage={stage}: {err}"));
    let read = read_some(resource, stage).await;
    assert!(read > 0, "expected decoded samples at stage={stage}");
    Resource::position(resource).as_secs_f64()
}

#[derive(Clone, Copy, Debug)]
pub(super) enum LocalKind {
    Mp3,
    HlsAac,
}

#[kithara::fixture]
pub(super) async fn audio_hls_pair() -> (CreatedHls, CreatedHls) {
    (open_audio_hls_server().await, open_audio_hls_server().await)
}

#[kithara::fixture]
pub(super) async fn aac_source() -> (TestServerHelper, url::Url) {
    create_packaged_single_variant_fixture(AudioCodec::AacLc).await
}

#[kithara::fixture]
pub(super) async fn flac_source() -> (TestServerHelper, url::Url) {
    create_packaged_single_variant_fixture(AudioCodec::Flac).await
}

pub(super) async fn registered_mp3(
    tone_mp3: &'static [u8],
    suffix: Option<&str>,
) -> (TestServerHelper, url::Url) {
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(tone_mp3.to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let url = suffix.map_or_else(|| handle.url(), |s| handle.child_url(s));
    (helper, url)
}

#[kithara::fixture]
pub(super) async fn mp3_extension(tone_mp3: &'static [u8]) -> (TestServerHelper, url::Url) {
    registered_mp3(tone_mp3, Some("track.mp3")).await
}

#[kithara::fixture]
pub(super) async fn mp3_no_extension(tone_mp3: &'static [u8]) -> (TestServerHelper, url::Url) {
    registered_mp3(tone_mp3, None).await
}

pub(super) async fn local_source(kind: LocalKind) -> (TestServerHelper, url::Url) {
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
    (helper, url)
}

#[kithara::fixture]
pub(super) async fn local_mp3() -> (TestServerHelper, url::Url) {
    local_source(LocalKind::Mp3).await
}

#[kithara::fixture]
pub(super) async fn local_hls() -> (TestServerHelper, url::Url) {
    local_source(LocalKind::HlsAac).await
}
