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
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{EncryptionRequest, PackagedSignal},
};
use kithara_platform::tokio::task::spawn_blocking;
use kithara_stream::AudioCodec;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::common::{
    CHANNELS, FREQ_HZ, PhaseDrift, SAMPLE_RATE, SinePhaseSpec, scripted_phase_scan,
};

const SEGMENT_DURATION_SECS: f64 = 2.0;
const SEGMENTS_PER_VARIANT: usize = 30;
const VARIANT_COUNT: usize = 3;
/// Highest-quality variant index. Production zvuk masters put FLAC lossless
/// on the top variant above the AAC ladder; [`Fixture::AacWithFlacTop`]
/// mirrors that and `Manual(TOP_VARIANT)` selects it.
const TOP_VARIANT: usize = VARIANT_COUNT - 1;

/// AES-128 key/IV for the encrypted (DRM) fixtures. The test server encrypts
/// segments with this key and serves it at the `#EXT-X-KEY` URI, so the value
/// only needs to be a valid 16-byte key — the client fetches and decrypts with
/// the same bytes. `30313233…` = ASCII `b"0123456789abcdef"`.
const AES_KEY_HEX: &str = "30313233343536373839616263646566";
const AES_IV_HEX: &str = "00000000000000000000000000000000";

#[derive(Debug, Clone, Copy)]
enum Codec {
    AacLc,
    AacHeV2,
    Flac,
}

#[derive(Debug, Clone, Copy)]
enum Fixture {
    /// One codec across all variants.
    Single(Codec),
    /// AAC on the lower variants + FLAC lossless on the top variant, the same
    /// 440 Hz sine encoded into every variant. Mirrors production masters where
    /// "switch to highest quality" is a cross-codec AAC→FLAC variant change.
    AacWithFlacTop,
}

fn e2e(mode: AbrMode) -> Vec<(AbrMode, f64)> {
    vec![(mode, 0.0)]
}

/// Single seek, no variant change (legacy `*_1seek` coverage).
fn one_seek() -> Vec<(AbrMode, f64)> {
    vec![(AbrMode::Manual(0), 0.0), (AbrMode::Manual(0), 0.5)]
}

/// Play the lower variant through the first half, then switch to the top
/// variant and play to the end. On [`Fixture::AacWithFlacTop`] this is the
/// production "switch to highest quality" AAC→FLAC change.
fn switch_to_top_mid() -> Vec<(AbrMode, f64)> {
    vec![
        (AbrMode::Manual(0), 0.0),
        (AbrMode::Manual(TOP_VARIANT), 0.5),
    ]
}

/// Eight scripted seeks with variant switches at every step (increasing
/// fractions partition the track, so total read ≈ one pass). Replaces the
/// legacy `*_10seek` random cycling with a deterministic, switch-heavy script.
fn multi_switch() -> Vec<(AbrMode, f64)> {
    vec![
        (AbrMode::Manual(0), 0.05),
        (AbrMode::Manual(TOP_VARIANT), 0.18),
        (AbrMode::Manual(1), 0.31),
        (AbrMode::Manual(TOP_VARIANT), 0.44),
        (AbrMode::Manual(0), 0.57),
        (AbrMode::Manual(TOP_VARIANT), 0.70),
        (AbrMode::Manual(1), 0.83),
        (AbrMode::Manual(TOP_VARIANT), 0.93),
    ]
}

/// Eight scripted seeks that keep `mode` constant (Auto stays Auto): replaces
/// the legacy auto `*_10seek` which seeked without switching variants.
fn seeks_no_switch(mode: AbrMode) -> Vec<(AbrMode, f64)> {
    (1..=8).map(|i| (mode, f64::from(i) * 0.11)).collect()
}

fn build_fixture(fixture: Fixture, bit_rate: Option<u64>, drm: bool) -> HlsFixtureBuilder {
    let b = HlsFixtureBuilder::new()
        .variant_count(VARIANT_COUNT)
        .segments_per_variant(SEGMENTS_PER_VARIANT)
        .segment_duration_secs(SEGMENT_DURATION_SECS);
    let b = match fixture {
        Fixture::Single(Codec::AacLc) => {
            b.packaged_audio_sine_aac_lc(SAMPLE_RATE, CHANNELS, FREQ_HZ)
        }
        Fixture::Single(Codec::AacHeV2) => b.packaged_audio_signal_aac_he_v2(
            SAMPLE_RATE,
            CHANNELS,
            PackagedSignal::Sine { freq_hz: FREQ_HZ },
        ),
        Fixture::Single(Codec::Flac) => b.packaged_audio_sine_flac(SAMPLE_RATE, CHANNELS, FREQ_HZ),
        Fixture::AacWithFlacTop => b
            .packaged_audio_sine_aac_lc(SAMPLE_RATE, CHANNELS, FREQ_HZ)
            .override_variant_codec(TOP_VARIANT, AudioCodec::Flac),
    };
    let b = b.packaged_audio_bit_rate(bit_rate);
    if drm {
        b.encryption(EncryptionRequest {
            key_hex: AES_KEY_HEX.to_owned(),
            iv_hex: Some(AES_IV_HEX.to_owned()),
        })
    } else {
        b
    }
}

async fn run_case(
    fixture: Fixture,
    backend: DecoderBackend,
    ephemeral: bool,
    drm: bool,
    scenario: Vec<(AbrMode, f64)>,
    bit_rate: Option<u64>,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(build_fixture(fixture, bit_rate, drm))
        .await
        .expect("create sine HLS fixture");

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();
    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.cache_capacity = Some(NonZeroUsize::new(SEGMENTS_PER_VARIANT + 10).expect("nonzero"));
        store.is_ephemeral = true;
    }
    let initial_mode = scenario.first().map_or(AbrMode::default(), |&(m, _)| m);
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
        ?fixture,
        ?backend,
        ?initial_mode,
        ephemeral,
        drm,
        steps = scenario.len(),
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

    let drifts = spawn_blocking(move || -> Vec<PhaseDrift> {
        let sine = SinePhaseSpec::default_440();
        let mut current = initial_mode;
        scripted_phase_scan(&mut audio, sine, total_frames_truth, &scenario, |&mode| {
            if mode != current {
                if let Some(h) = abr_handle.as_ref()
                    && let Err(e) = h.set_mode(mode)
                {
                    warn!(?e, ?mode, "variant switch failed");
                }
                current = mode;
            }
        })
    })
    .await
    .expect("spawn_blocking joined");

    assert!(
        drifts.is_empty(),
        "phase continuity broken on {} scan(s) (fixture={fixture:?} backend={backend:?} drm={drm}): {drifts:?}",
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
    case::sentinel_aac_he_v2_apple_eph_manual_multi(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Apple,
        true,
        false,
        multi_switch(),
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_eph_manual_e2e(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Apple,
        true,
        false,
        e2e(AbrMode::Manual(0)),
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lc_apple_eph_manual_multi(
        Fixture::Single(Codec::AacLc),
        DecoderBackend::Apple,
        true,
        false,
        multi_switch(),
        Some(320_000),
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lc_apple_eph_manual_e2e(
        Fixture::Single(Codec::AacLc),
        DecoderBackend::Apple,
        true,
        false,
        e2e(AbrMode::Manual(0)),
        Some(320_000),
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple_eph_manual_multi(
        Fixture::Single(Codec::Flac),
        DecoderBackend::Apple,
        true,
        false,
        multi_switch(),
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple_eph_manual_e2e(
        Fixture::Single(Codec::Flac),
        DecoderBackend::Apple,
        true,
        false,
        e2e(AbrMode::Manual(0)),
        None,
    )
)]
#[case::aac_he_v2_symphonia_eph_manual_multi(
    Fixture::Single(Codec::AacHeV2),
    DecoderBackend::Symphonia,
    true,
    false,
    multi_switch(),
    None
)]
#[case::aac_he_v2_symphonia_eph_manual_e2e(
    Fixture::Single(Codec::AacHeV2),
    DecoderBackend::Symphonia,
    true,
    false,
    e2e(AbrMode::Manual(0)),
    None
)]
#[case::aac_lc_symphonia_eph_manual_multi(
    Fixture::Single(Codec::AacLc),
    DecoderBackend::Symphonia,
    true,
    false,
    multi_switch(),
    Some(320_000)
)]
#[case::aac_lc_symphonia_eph_manual_e2e(
    Fixture::Single(Codec::AacLc),
    DecoderBackend::Symphonia,
    true,
    false,
    e2e(AbrMode::Manual(0)),
    Some(320_000)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_eph_auto_multi(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Apple,
        true,
        false,
        seeks_no_switch(AbrMode::Auto(Some(1))),
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_eph_auto_e2e(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Apple,
        true,
        false,
        e2e(AbrMode::Auto(Some(1))),
        None,
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_he_v2_apple_mmap_manual_multi(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Apple,
        false,
        false,
        multi_switch(),
        None,
    )
)]
#[case::prod_flac_top_sustained_symphonia(
    Fixture::AacWithFlacTop,
    DecoderBackend::Symphonia,
    true,
    false,
    e2e(AbrMode::Manual(TOP_VARIANT)),
    None
)]
#[case::flac_only_top_sustained_symphonia(
    Fixture::Single(Codec::Flac),
    DecoderBackend::Symphonia,
    true,
    false,
    e2e(AbrMode::Manual(TOP_VARIANT)),
    None
)]
#[cfg_attr(
    target_os = "android",
    case::aac_he_v2_android_eph_manual_multi(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Android,
        true,
        false,
        multi_switch(),
        None,
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_he_v2_android_eph_manual_e2e(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Android,
        true,
        false,
        e2e(AbrMode::Manual(0)),
        None,
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_lc_android_eph_manual_multi(
        Fixture::Single(Codec::AacLc),
        DecoderBackend::Android,
        true,
        false,
        multi_switch(),
        Some(320_000),
    )
)]
#[cfg_attr(
    target_os = "android",
    case::flac_android_eph_manual_multi(
        Fixture::Single(Codec::Flac),
        DecoderBackend::Android,
        true,
        false,
        multi_switch(),
        None,
    )
)]
#[cfg_attr(
    target_os = "android",
    case::aac_he_v2_android_eph_auto_multi(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Android,
        true,
        false,
        seeks_no_switch(AbrMode::Auto(Some(1))),
        None,
    )
)]
async fn phase_continuity_hls(
    #[case] fixture: Fixture,
    #[case] backend: DecoderBackend,
    #[case] ephemeral: bool,
    #[case] drm: bool,
    #[case] scenario: Vec<(AbrMode, f64)>,
    #[case] bit_rate: Option<u64>,
) {
    run_case(fixture, backend, ephemeral, drm, scenario, bit_rate).await;
}

/// Pins real, uncompensated decoder-warmup phase drifts at HLS seek/switch
/// seams. None of these break within a single steady codec — they surface only
/// where the decoder is repositioned, because the per-codec decode delay is not
/// reflected in the output timeline:
///
/// - **Cross-codec switch** (`cross_codec_switch_*`): switching to the
///   highest-quality variant (AAC→FLAC, mirroring zvuk masters) drops
///   ~1024 samples (one AAC access unit) at the seam. `default_priming_frames`
///   is `AacLc == 1024`, `Flac == 0`; AAC output is offset by 1024 vs FLAC, so
///   the seam jumps `≈ 21.878 samples = 1024 mod (44100/440)`. Both backends,
///   with and without DRM.
/// - **In-variant seek** (`he_v2_in_variant_seek_apple`): a plain seek with no
///   variant change in HE-AAC v2 (Apple) drifts ~40 samples post-seek — the
///   SBR/PS warmup applied on the variant-switch recreate path is missing on
///   the plain in-variant seek path.
///
/// `#[ignore]`d, not deleted: acceptance targets for the replanned HLS
/// decoder-warmup / per-codec algo-delay work (decoder owns warmup). Drop the
/// `#[ignore]` when that fix lands. Run with `--run-ignored`.
///
/// NOTE: this does NOT cover the separately reported production symptom — a
/// 1–5 s forward position jump every 10–30 s during sustained FLAC playback on
/// the real-time (cpal) player. That is invisible to offline `Audio::read()`
/// pulls (which spin on `Pending` and never skip content). A player-loop offline
/// repro (`kithara_play::flac_realtime_player_continuity`, driving the real
/// `PlayerProcessor` via `OfflinePlayer::render`) was built and stays green
/// across sustained FLAC, the AAC→FLAC switch, 48 kHz resampling, and forced
/// underruns — so the symptom needs true real-time cpal pressure or the live
/// production stream, not a paced offline pull.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(25)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[ignore = "pins real regression — uncompensated decoder warmup at HLS seek/switch seams (cross-codec AAC→FLAC ~1024 samples; he_v2 in-variant seek ~40 samples); unignore when HLS decoder-warmup / per-codec algo-delay fix lands"]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::cross_codec_switch_apple_drm(
        Fixture::AacWithFlacTop,
        DecoderBackend::Apple,
        true,
        switch_to_top_mid(),
    )
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::cross_codec_switch_apple_plain(
        Fixture::AacWithFlacTop,
        DecoderBackend::Apple,
        false,
        switch_to_top_mid(),
    )
)]
#[case::cross_codec_switch_symphonia_drm(
    Fixture::AacWithFlacTop,
    DecoderBackend::Symphonia,
    true,
    switch_to_top_mid()
)]
#[case::cross_codec_switch_symphonia_plain(
    Fixture::AacWithFlacTop,
    DecoderBackend::Symphonia,
    false,
    switch_to_top_mid()
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::he_v2_in_variant_seek_apple(
        Fixture::Single(Codec::AacHeV2),
        DecoderBackend::Apple,
        false,
        one_seek(),
    )
)]
async fn phase_continuity_hls_known_warmup_drift(
    #[case] fixture: Fixture,
    #[case] backend: DecoderBackend,
    #[case] drm: bool,
    #[case] scenario: Vec<(AbrMode, f64)>,
) {
    let bit_rate = matches!(fixture, Fixture::AacWithFlacTop).then_some(320_000);
    run_case(fixture, backend, true, drm, scenario, bit_rate).await;
}
