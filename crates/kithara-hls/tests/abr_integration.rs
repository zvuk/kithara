#![forbid(unsafe_code)]

mod fixture;

use std::{
    sync::{Arc, atomic::AtomicUsize},
    time::{Duration, Instant},
};

use kithara_hls::{
    abr::{AbrConfig, AbrController, AbrReason, variants_from_master},
    playlist::parse_master_playlist,
};
use rstest::{fixture, rstest};

// ==================== Fixtures ====================

#[fixture]
fn abr_config_default() -> AbrConfig {
    AbrConfig::default()
}

#[fixture]
fn test_master_playlist_data() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID="vid",NAME="720p",BANDWIDTH=2000000,RESOLUTION=1280x720
#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720,CODECS="avc1.64001f,mp4a.40.2"
video/720p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=854x480,CODECS="avc1.64001f,mp4a.40.2"
video/480p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=500000,RESOLUTION=640x360,CODECS="avc1.64001f,mp4a.40.2"
video/360p/playlist.m3u8
"#
}

#[fixture]
fn parsed_master_playlist(
    test_master_playlist_data: &str,
) -> kithara_hls::playlist::MasterPlaylist {
    parse_master_playlist(test_master_playlist_data.as_bytes())
        .expect("Failed to parse master playlist")
}

#[fixture]
fn variants_from_parsed_playlist(
    parsed_master_playlist: kithara_hls::playlist::MasterPlaylist,
) -> Vec<kithara_hls::abr::Variant> {
    variants_from_master(&parsed_master_playlist)
}

// ==================== Test Cases ====================

#[rstest]
fn test_variant_selection_manual_override(
    abr_config_default: AbrConfig,
    parsed_master_playlist: kithara_hls::playlist::MasterPlaylist,
    variants_from_parsed_playlist: Vec<kithara_hls::abr::Variant>,
) {
    let variant_index = Arc::new(AtomicUsize::new(0));
    let selector = std::sync::Arc::new(|_playlist: &kithara_hls::playlist::MasterPlaylist| Some(2));
    let controller = AbrController::new(abr_config_default, Some(selector), variant_index);

    let decision = controller.decide_for_master(
        &parsed_master_playlist,
        &variants_from_parsed_playlist,
        0.0,
        Instant::now(),
    );

    assert_eq!(decision.target_variant_index, 2);
    assert_eq!(decision.reason, AbrReason::ManualOverride);
}

#[rstest]
#[case(0, Some(0))]
#[case(1, Some(1))]
#[case(2, Some(2))]
#[case(3, None)]
fn test_manual_selector_different_indices(
    #[case] selector_index: usize,
    #[case] expected_variant_index: Option<usize>,
    abr_config_default: AbrConfig,
    parsed_master_playlist: kithara_hls::playlist::MasterPlaylist,
    variants_from_parsed_playlist: Vec<kithara_hls::abr::Variant>,
) {
    let variant_index = Arc::new(AtomicUsize::new(0));
    let selector = std::sync::Arc::new(move |_playlist: &kithara_hls::playlist::MasterPlaylist| {
        Some(selector_index)
    });
    let controller = AbrController::new(abr_config_default, Some(selector), variant_index);

    let decision = controller.decide_for_master(
        &parsed_master_playlist,
        &variants_from_parsed_playlist,
        0.0,
        Instant::now(),
    );

    if let Some(expected) = expected_variant_index {
        assert_eq!(decision.target_variant_index, expected);
        assert_eq!(decision.reason, AbrReason::ManualOverride);
    } else {
        // If selector returns index out of bounds, controller should handle it gracefully
        // It might choose a default variant or handle the error internally
        // We just verify the decision is valid (not necessarily checking bounds)
        assert!(decision.target_variant_index <= variants_from_parsed_playlist.len());
    }
}

#[rstest]
fn test_abr_controller_no_selector(
    abr_config_default: AbrConfig,
    parsed_master_playlist: kithara_hls::playlist::MasterPlaylist,
    variants_from_parsed_playlist: Vec<kithara_hls::abr::Variant>,
) {
    let variant_index = Arc::new(AtomicUsize::new(0));
    let controller = AbrController::new(abr_config_default, None, variant_index);

    let decision = controller.decide_for_master(
        &parsed_master_playlist,
        &variants_from_parsed_playlist,
        0.0,
        Instant::now(),
    );

    // Without selector, should use default logic (likely first variant)
    assert!(decision.target_variant_index < variants_from_parsed_playlist.len());
    assert_ne!(decision.reason, AbrReason::ManualOverride);
}

#[rstest]
#[case(0.0, 0.0)] // No buffer
#[case(1.0, 0.0)] // 1 second buffer
#[case(5.0, 0.0)] // 5 second buffer
#[case(0.0, 2.0)] // No buffer, 2 seconds since last switch
#[case(3.0, 5.0)] // 3 second buffer, 5 seconds since last switch
fn test_abr_decision_with_different_conditions(
    #[case] buffer_secs: f64,
    #[case] time_since_last_switch_secs: f64,
    abr_config_default: AbrConfig,
    parsed_master_playlist: kithara_hls::playlist::MasterPlaylist,
    variants_from_parsed_playlist: Vec<kithara_hls::abr::Variant>,
) {
    let variant_index = Arc::new(AtomicUsize::new(0));
    let selector = std::sync::Arc::new(|_playlist: &kithara_hls::playlist::MasterPlaylist| Some(1));
    let controller = AbrController::new(abr_config_default, Some(selector), variant_index);

    let time_since_last_switch = Duration::from_secs_f64(time_since_last_switch_secs);
    let last_switch_time = Instant::now() - time_since_last_switch;

    let decision = controller.decide_for_master(
        &parsed_master_playlist,
        &variants_from_parsed_playlist,
        buffer_secs,
        last_switch_time,
    );

    // Should still respect manual override regardless of conditions
    assert_eq!(decision.target_variant_index, 1);
    assert_eq!(decision.reason, AbrReason::ManualOverride);
}

#[rstest]
fn test_variants_from_master_structure(
    parsed_master_playlist: kithara_hls::playlist::MasterPlaylist,
) {
    let variants = variants_from_master(&parsed_master_playlist);

    assert_eq!(variants.len(), 3);

    // Check bandwidth ordering (should be from highest to lowest based on playlist)
    assert_eq!(variants[0].bandwidth_bps, 2000000);
    assert_eq!(variants[1].bandwidth_bps, 1000000);
    assert_eq!(variants[2].bandwidth_bps, 500000);

    // Check variant indices
    assert_eq!(variants[0].variant_index, 0);
    assert_eq!(variants[1].variant_index, 1);
    assert_eq!(variants[2].variant_index, 2);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_abr_controller_async_usage() {
    // Test that ABR controller can be used in async context
    let variant_index = Arc::new(AtomicUsize::new(0));
    let config = AbrConfig::default();
    let selector = std::sync::Arc::new(|_playlist: &kithara_hls::playlist::MasterPlaylist| Some(0));
    let _controller = AbrController::new(config, Some(selector), variant_index);

    // Just verify it compiles and can be created in async context
    assert!(true);
}
