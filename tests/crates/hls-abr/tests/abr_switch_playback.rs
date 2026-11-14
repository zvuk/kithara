use kithara::{decode::DecoderBackend, platform::time::Duration};
use kithara_integration_tests::{TestServerHelper, mixed_encrypted, mixed_plain, served_mp3};
use kithara_test_utils::{TestTempDir, temp_dir};
use packaged::create_packaged_abr_fixture;
use url::Url;

use super::abr_playback::{packaged, seek};

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(3)
)]
async fn abr_switch_on_production_ladder_does_not_hang(
    temp_dir: TestTempDir,
    #[future(awt)] mixed_plain: (TestServerHelper, Url),
) {
    packaged::abr_switch_on_production_ladder_does_not_hang(temp_dir, mixed_plain).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(3),
    tracing("kithara_abr=debug,kithara_audio=debug,kithara_hls=debug,kithara_stream=debug")
)]
async fn packaged_abr_switch_keeps_player_continuity(
    temp_dir: TestTempDir,
    #[future(awt)] create_packaged_abr_fixture: (TestServerHelper, Url),
) {
    packaged::packaged_abr_switch_keeps_player_continuity(temp_dir, create_packaged_abr_fixture)
        .await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(5)
)]
async fn manual_cross_codec_switch_sustains_post_switch_playback(
    temp_dir: TestTempDir,
    #[future(awt)] mixed_plain: (TestServerHelper, Url),
) {
    packaged::manual_cross_codec_switch_sustains_post_switch_playback(temp_dir, mixed_plain).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
#[cfg_attr(not(target_os = "android"), case::drm_abr_auto_sw(true, true, DecoderBackend::Symphonia, mixed_encrypted().await))]
#[cfg_attr(target_os = "android", case::drm_abr_auto_android(true, true, DecoderBackend::default(), mixed_encrypted().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drm_abr_auto_hw(true, true, DecoderBackend::Apple, mixed_encrypted().await)
)]
#[cfg_attr(not(target_os = "android"), case::hls_abr_auto_sw(false, true, DecoderBackend::Symphonia, mixed_plain().await))]
#[cfg_attr(target_os = "android", case::hls_abr_auto_android(false, true, DecoderBackend::default(), mixed_plain().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_abr_auto_hw(false, true, DecoderBackend::Apple, mixed_plain().await)
)]
#[cfg_attr(not(target_os = "android"), case::drm_manual_v0_sw(true, false, DecoderBackend::Symphonia, mixed_encrypted().await))]
#[cfg_attr(target_os = "android", case::drm_manual_v0_android(true, false, DecoderBackend::default(), mixed_encrypted().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drm_manual_v0_hw(true, false, DecoderBackend::Apple, mixed_encrypted().await)
)]
#[cfg_attr(not(target_os = "android"), case::hls_manual_v0_sw(false, false, DecoderBackend::Symphonia, mixed_plain().await))]
#[cfg_attr(target_os = "android", case::hls_manual_v0_android(false, false, DecoderBackend::default(), mixed_plain().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_manual_v0_hw(false, false, DecoderBackend::Apple, mixed_plain().await)
)]
async fn stream_continues_after_seek(
    temp_dir: TestTempDir,
    #[case] encrypted: bool,
    #[case] abr_auto: bool,
    #[case] backend: DecoderBackend,
    #[case] prepared: (TestServerHelper, Url),
) {
    seek::stream_continues_after_seek(temp_dir, encrypted, abr_auto, backend, prepared).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(3)
)]
async fn fixed_variant_on_production_ladder_plays_without_hang(
    temp_dir: TestTempDir,
    #[future(awt)] mixed_plain: (TestServerHelper, Url),
) {
    seek::fixed_variant_on_production_ladder_plays_without_hang(temp_dir, mixed_plain).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5),
    tracing("kithara_audio=warn,kithara_hls=warn,symphonia_format_isomp4=warn")
)]
#[case::drm(true, mixed_encrypted().await)]
#[case::hls(false, mixed_plain().await)]
async fn seek_after_eof_mmap_produces_samples(
    temp_dir: TestTempDir,
    #[case] encrypted: bool,
    #[case] prepared: (TestServerHelper, Url),
) {
    seek::seek_after_eof_mmap_produces_samples(temp_dir, encrypted, prepared).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
async fn mp3_stream_continues_after_seek(
    temp_dir: TestTempDir,
    #[future(awt)] served_mp3: (TestServerHelper, Url),
) {
    seek::mp3_stream_continues_after_seek(temp_dir, served_mp3).await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(3),
    tracing("kithara_audio=info,kithara_hls=info")
)]
async fn abr_frozen_during_seek_resumes_after(temp_dir: TestTempDir) {
    seek::abr_frozen_during_seek_resumes_after(temp_dir).await;
}
