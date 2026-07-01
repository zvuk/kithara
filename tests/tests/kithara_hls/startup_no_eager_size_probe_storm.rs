use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::DataMode,
};
use kithara_platform::{CancelToken, time::Duration, tokio::task::spawn_blocking};
use tracing::info;

const VARIANT_COUNT: usize = 3;
const SEGMENTS_PER_VARIANT: usize = 6;
const ACTIVE_VARIANT: usize = 0;
const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;

#[derive(Clone, Copy, Debug)]
enum StartupFixture {
    WavFileLike,
    FlacFmp4,
}

impl StartupFixture {
    fn build(self) -> HlsFixtureBuilder {
        let builder = HlsFixtureBuilder::new()
            .variant_count(VARIANT_COUNT)
            .segments_per_variant(SEGMENTS_PER_VARIANT)
            .segment_duration_secs(0.25)
            .variant_bandwidths(vec![128_000, 192_000, 256_000]);
        match self {
            Self::WavFileLike => builder.data_mode(DataMode::SawWav {
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS,
            }),
            Self::FlacFmp4 => builder.packaged_audio_flac(SAMPLE_RATE, CHANNELS),
        }
    }

    fn media_info(self) -> MediaInfo {
        match self {
            Self::WavFileLike => MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav)),
            Self::FlacFmp4 => MediaInfo::new(Some(AudioCodec::Flac), Some(ContainerFormat::Fmp4)),
        }
    }
}

fn variant_size_probe_count(helper: &TestServerHelper, token: &str, variant: usize) -> u64 {
    (0..SEGMENTS_PER_VARIANT)
        .map(|segment| helper.size_probe_count(token, variant, segment))
        .sum()
}

/// Guard for the S13 lazy-size startup contract.
///
/// Startup must not issue eager size probes for either file-like WAV or
/// segment-aware fMP4. Exact sizes are resolved only when a seek/read path
/// actually needs them.
#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(30)))]
#[case::wav_file_like(StartupFixture::WavFileLike)]
#[case::flac_fmp4(StartupFixture::FlacFmp4)]
#[cfg(not(target_arch = "wasm32"))]
async fn startup_issues_no_eager_size_probe_storm(#[case] fixture: StartupFixture) {
    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(fixture.build())
        .await
        .expect("create HLS fixture");
    let url = created.master_url();
    info!(
        ?fixture,
        %url,
        variants = VARIANT_COUNT,
        segments = SEGMENTS_PER_VARIANT,
        "HLS server ready"
    );

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(ACTIVE_VARIANT))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(fixture.media_info())
        .decoder_backend(DecoderBackend::Symphonia)
        .block_on_underrun(true)
        .build();

    // NOTE: this is the historical storm seam. Keep creation and first-frame
    // decode ahead of the counter read so both construction-time and startup
    // read-time probes are visible.
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    // NOTE: drive to the first decoded frame without a sleep, so the active
    // variant prefix is loaded before reading counters.
    let audio = spawn_blocking(move || {
        let mut buf = vec![0.0f32; 4096];
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                info!(frames = count.get(), "first decoded audio frame");
            }
            Ok(ReadOutcome::Pending { .. }) => {
                panic!("read returned Pending with block_on_underrun before first frame");
            }
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("read returned Eof before any frame");
            }
            Err(e) => panic!("read error before first frame: {e}"),
        }
        audio
    })
    .await
    .expect("spawn_blocking first-frame read panicked");
    drop(audio);

    let token = created.token();
    let total: u64 = (0..VARIANT_COUNT)
        .map(|variant| variant_size_probe_count(&helper, token, variant))
        .sum();
    let active = variant_size_probe_count(&helper, token, ACTIVE_VARIANT);
    let non_active: Vec<u64> = (0..VARIANT_COUNT)
        .filter(|&v| v != ACTIVE_VARIANT)
        .map(|v| variant_size_probe_count(&helper, token, v))
        .collect();

    info!(
        total,
        active,
        ?non_active,
        "size-probe counts before first audio"
    );

    let storm_bound =
        u64::try_from(VARIANT_COUNT * SEGMENTS_PER_VARIANT).expect("small fixture size");

    // NOTE: no all-variant storm.
    assert!(
        total < storm_bound,
        "eager size-probe storm: server saw {total} size-probes before first audio \
         for {fixture:?} \
         (active variant {ACTIVE_VARIANT} = {active}, non-active = {non_active:?}); \
         expected < {storm_bound} (= {VARIANT_COUNT} variants * {SEGMENTS_PER_VARIANT} segments). \
         The startup estimator is probing every segment of every variant.",
    );

    assert_eq!(
        total, 0,
        "HLS startup must not issue eager size-probes before first audio \
         for {fixture:?} \
         (active variant {ACTIVE_VARIANT} = {active}, non-active = {non_active:?})",
    );

    // NOTE: non-active variants must never be probed at startup.
    let non_active_total: u64 = non_active.iter().sum();
    assert_eq!(
        non_active_total, 0,
        "non-active variants received {non_active_total} size-probes before first audio \
         (per-variant = {non_active:?}); expected 0 — only the active variant's prefix \
         should resolve. The startup estimator is probing inactive variants.",
    );
}
