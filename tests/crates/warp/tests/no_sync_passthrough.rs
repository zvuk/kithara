#![cfg(not(target_arch = "wasm32"))]

use kithara::{platform::time::Duration, warp::StretchKind};
use kithara_integration_tests::kithara;
use kithara_test_fixtures::integration_fixtures::shifted_pitch;
use kithara_warp_tests::mock::{
    marked_source_pcm, run_active_stretch, run_no_sync_passthrough, source_pcm,
};

#[kithara::test(tokio, serial, timeout(Duration::from_secs(30)), hang_timeout_secs(5))]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case(StretchKind::Bungee)
)]
async fn no_sync_unity_player_and_queue_playback_is_bit_exact_and_cochlea_clean(
    source_pcm: &'static [u8],
    #[case] backend: StretchKind,
) {
    run_no_sync_passthrough(source_pcm, backend, false).await;
}

#[kithara::test(tokio, serial, timeout(Duration::from_secs(30)), hang_timeout_secs(5))]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case(StretchKind::Bungee)
)]
async fn no_sync_active_keylock_is_continuous_and_preserves_pitch(
    source_pcm: &'static [u8],
    marked_source_pcm: &'static [u8],
    shifted_pitch: Vec<f32>,
    #[case] backend: StretchKind,
) {
    run_active_stretch(source_pcm, marked_source_pcm, shifted_pitch, backend, false).await;
}

#[kithara::test(tokio, serial, timeout(Duration::from_secs(60)), hang_timeout_secs(5))]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case(StretchKind::Bungee)
)]
#[ignore = "writes opt-in listening artifacts; run explicitly with KITHARA_AUDIO_ARTIFACT_DIR"]
async fn record_no_sync_unity_playback_artifacts(
    source_pcm: &'static [u8],
    #[case] backend: StretchKind,
) {
    run_no_sync_passthrough(source_pcm, backend, true).await;
}

#[kithara::test(tokio, serial, timeout(Duration::from_secs(60)), hang_timeout_secs(5))]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case(StretchKind::Bungee)
)]
#[ignore = "writes opt-in listening artifacts; run explicitly with KITHARA_AUDIO_ARTIFACT_DIR"]
async fn record_no_sync_active_keylock_artifacts(
    source_pcm: &'static [u8],
    marked_source_pcm: &'static [u8],
    shifted_pitch: Vec<f32>,
    #[case] backend: StretchKind,
) {
    run_active_stretch(source_pcm, marked_source_pcm, shifted_pitch, backend, true).await;
}
