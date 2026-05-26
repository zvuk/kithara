use std::{num::NonZeroUsize, time::Duration};

use kithara::{
    abr::AbrMode,
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_decode::DecoderBackend;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::PackagedSignal,
};
use kithara_platform::tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::common::{
    CHANNELS, FREQ_HZ, PhaseDrift, SAMPLE_RATE, SinePhaseSpec, e2e_phase_scan, seek_phase_scan,
};

const SEGMENT_DURATION_SECS: f64 = 2.0;
const SEGMENTS_PER_VARIANT: usize = 30;
const VARIANT_COUNT: usize = 3;

#[derive(Debug, Clone, Copy)]
enum Codec {
    AacLc,
    AacHeV2,
    Flac,
}

fn build_fixture(codec: Codec, bit_rate: Option<u64>) -> HlsFixtureBuilder {
    let b = HlsFixtureBuilder::new()
        .variant_count(VARIANT_COUNT)
        .segments_per_variant(SEGMENTS_PER_VARIANT)
        .segment_duration_secs(SEGMENT_DURATION_SECS);
    let b = match codec {
        Codec::AacLc => b.packaged_audio_sine_aac_lc(SAMPLE_RATE, CHANNELS, FREQ_HZ),
        Codec::AacHeV2 => b.packaged_audio_signal_aac_he_v2(
            SAMPLE_RATE,
            CHANNELS,
            PackagedSignal::Sine { freq_hz: FREQ_HZ },
        ),
        Codec::Flac => b.packaged_audio_sine_flac(SAMPLE_RATE, CHANNELS, FREQ_HZ),
    };
    b.packaged_audio_bit_rate(bit_rate)
}

async fn run_case(
    codec: Codec,
    backend: DecoderBackend,
    ephemeral: bool,
    initial_mode: AbrMode,
    seek_count: usize,
    bit_rate: Option<u64>,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(build_fixture(codec, bit_rate))
        .await
        .expect("create sine HLS fixture");

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();
    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.cache_capacity = Some(NonZeroUsize::new(SEGMENTS_PER_VARIANT + 10).expect("nonzero"));
        store.is_ephemeral = true;
    }
    let hls_config = HlsConfig::for_url(created.master_url())
        .store(store)
        .cancel(cancel)
        .initial_abr_mode(initial_mode)
        .build();
    let audio_config = AudioConfig::<Hls>::for_stream(hls_config)
        .decoder_backend(backend)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(audio_config)
        .await
        .expect("create Audio<Stream<Hls>>");

    let total_secs = audio
        .duration()
        .expect("HLS sine fixture should report duration")
        .as_secs_f64();
    info!(
        ?codec,
        ?backend,
        ?initial_mode,
        ephemeral,
        seek_count,
        total_secs,
        "fixture ready"
    );
    assert!(
        total_secs > 30.0 && total_secs < 120.0,
        "fixture duration out of sane range: {total_secs:.1}s",
    );
    let total_frames_truth = (total_secs * f64::from(SAMPLE_RATE)) as u64;

    let aspec = audio.spec();
    assert_eq!(aspec.sample_rate, SAMPLE_RATE);
    assert_eq!(u32::from(aspec.channels), u32::from(CHANNELS));

    let abr_handle = audio.abr_handle();
    let abr_variants = abr_handle.as_ref().map_or(0, |h| h.variants().len());
    assert_eq!(
        abr_variants, VARIANT_COUNT,
        "fixture should expose {VARIANT_COUNT} variants, got {abr_variants}",
    );
    let cycle_manual = matches!(initial_mode, AbrMode::Manual(_));

    let drifts = spawn_blocking(move || -> Vec<PhaseDrift> {
        let sine = SinePhaseSpec::default_440();
        if seek_count == 0 {
            e2e_phase_scan(&mut audio, sine, total_frames_truth)
        } else {
            seek_phase_scan(
                &mut audio,
                sine,
                total_secs,
                seek_count,
                0xCAFE_F00D_DEAD_BEEFu64,
                |i| {
                    if cycle_manual && let Some(h) = abr_handle.as_ref() {
                        let target = i % abr_variants;
                        if let Err(e) = h.set_mode(AbrMode::Manual(target)) {
                            warn!(?e, target, "manual switch failed");
                        }
                    }
                },
            )
        }
    })
    .await
    .expect("spawn_blocking joined");

    assert!(
        drifts.is_empty(),
        "phase continuity broken on {} scan(s) (codec={codec:?} backend={backend:?} seek_count={seek_count}): {drifts:?}",
        drifts.len(),
    );
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::sentinel_aac_he_v2_apple_eph_manual_10seek(
        Codec::AacHeV2,
        DecoderBackend::Apple,
        true,
        AbrMode::Manual(0),
        10,
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_eph_manual_e2e(
        Codec::AacHeV2,
        DecoderBackend::Apple,
        true,
        AbrMode::Manual(0),
        0,
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_eph_manual_1seek(
        Codec::AacHeV2,
        DecoderBackend::Apple,
        true,
        AbrMode::Manual(0),
        1,
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lc_apple_eph_manual_10seek(
        Codec::AacLc,
        DecoderBackend::Apple,
        true,
        AbrMode::Manual(0),
        10,
        Some(320_000),
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lc_apple_eph_manual_e2e(
        Codec::AacLc,
        DecoderBackend::Apple,
        true,
        AbrMode::Manual(0),
        0,
        Some(320_000),
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple_eph_manual_10seek(
        Codec::Flac,
        DecoderBackend::Apple,
        true,
        AbrMode::Manual(0),
        10,
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple_eph_manual_e2e(
        Codec::Flac,
        DecoderBackend::Apple,
        true,
        AbrMode::Manual(0),
        0,
        None,
    )
)]
#[case::aac_he_v2_symphonia_eph_manual_10seek(
    Codec::AacHeV2,
    DecoderBackend::Symphonia,
    true,
    AbrMode::Manual(0),
    10,
    None
)]
#[case::aac_he_v2_symphonia_eph_manual_e2e(
    Codec::AacHeV2,
    DecoderBackend::Symphonia,
    true,
    AbrMode::Manual(0),
    0,
    None
)]
#[case::aac_lc_symphonia_eph_manual_10seek(
    Codec::AacLc,
    DecoderBackend::Symphonia,
    true,
    AbrMode::Manual(0),
    10,
    Some(320_000)
)]
#[case::aac_lc_symphonia_eph_manual_e2e(
    Codec::AacLc,
    DecoderBackend::Symphonia,
    true,
    AbrMode::Manual(0),
    0,
    Some(320_000)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_eph_auto_10seek(
        Codec::AacHeV2,
        DecoderBackend::Apple,
        true,
        AbrMode::Auto(Some(1)),
        10,
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_eph_auto_e2e(
        Codec::AacHeV2,
        DecoderBackend::Apple,
        true,
        AbrMode::Auto(Some(1)),
        0,
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_mmap_manual_10seek(
        Codec::AacHeV2,
        DecoderBackend::Apple,
        false,
        AbrMode::Manual(0),
        10,
        None,
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_he_v2_android_eph_manual_10seek(
        Codec::AacHeV2,
        DecoderBackend::Android,
        true,
        AbrMode::Manual(0),
        10,
        None,
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_he_v2_android_eph_manual_e2e(
        Codec::AacHeV2,
        DecoderBackend::Android,
        true,
        AbrMode::Manual(0),
        0,
        None,
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_lc_android_eph_manual_10seek(
        Codec::AacLc,
        DecoderBackend::Android,
        true,
        AbrMode::Manual(0),
        10,
        Some(320_000),
    )
)]
#[cfg_attr(
    target_os = "android",
    case::flac_android_eph_manual_10seek(
        Codec::Flac,
        DecoderBackend::Android,
        true,
        AbrMode::Manual(0),
        10,
        None,
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_he_v2_android_eph_auto_10seek(
        Codec::AacHeV2,
        DecoderBackend::Android,
        true,
        AbrMode::Auto(Some(1)),
        10,
        None,
    )
)]
async fn phase_continuity_hls(
    #[case] codec: Codec,
    #[case] backend: DecoderBackend,
    #[case] ephemeral: bool,
    #[case] initial_mode: AbrMode,
    #[case] seek_count: usize,
    #[case] bit_rate: Option<u64>,
) {
    run_case(
        codec,
        backend,
        ephemeral,
        initial_mode,
        seek_count,
        bit_rate,
    )
    .await;
}
