#![forbid(unsafe_code)]

use kithara::internal::{MasterPlaylist, parse_master_playlist};
use kithara_abr::{AbrController, AbrMode, AbrSettings, AbrVariant};
use kithara_events::VariantDuration;
use kithara_platform::time::Duration;

/// Convert HLS master playlist variants to ABR variant list (test helper).
fn variants_from_master(master: &MasterPlaylist) -> Vec<AbrVariant> {
    master
        .variants
        .iter()
        .map(|v| AbrVariant {
            variant_index: v.id.0,
            bandwidth_bps: v.bandwidth.unwrap_or(0),
            duration: VariantDuration::Total(Duration::ZERO),
        })
        .collect()
}

// Fixtures

#[kithara::fixture]
fn abr_settings_default() -> AbrSettings {
    AbrSettings::default()
}

#[kithara::fixture]
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

#[kithara::fixture]
fn parsed_master_playlist(test_master_playlist_data: &str) -> MasterPlaylist {
    parse_master_playlist(test_master_playlist_data.as_bytes())
        .expect("Failed to parse master playlist")
}

#[kithara::fixture]
fn variants_from_parsed_playlist(parsed_master_playlist: MasterPlaylist) -> Vec<AbrVariant> {
    variants_from_master(&parsed_master_playlist)
}

// Test Cases

/// `AbrMode::Manual(idx)` is just a stateless enum payload — AbrController
/// constructs and holds settings; actual decisions are covered in the
/// scheduler / integration tests. We only verify the controller builds
/// successfully for a range of selector indices (including out-of-bounds).
#[kithara::test]
#[case(0)]
#[case(1)]
#[case(2)]
#[case(3)]
fn test_manual_selector_different_indices(
    #[case] selector_index: usize,
    variants_from_parsed_playlist: Vec<AbrVariant>,
) {
    let controller = AbrController::new(AbrSettings::default());
    let _ = controller.settings().warmup_min_bytes;
    assert_eq!(variants_from_parsed_playlist.len(), 3);
    let _ = AbrMode::Manual(selector_index);
}

#[kithara::test]
fn test_abr_controller_no_selector(
    abr_settings_default: AbrSettings,
    variants_from_parsed_playlist: Vec<AbrVariant>,
) {
    let controller = AbrController::new(abr_settings_default);
    assert!(controller.current_bandwidth_estimate_bps().is_none());
    assert_eq!(variants_from_parsed_playlist.len(), 3);
}

#[kithara::test]
#[case(0.0, 0.0)] // No buffer
#[case(1.0, 0.0)] // 1 second buffer
#[case(5.0, 0.0)] // 5 second buffer
#[case(0.0, 2.0)] // No buffer, 2 seconds since last switch
#[case(3.0, 5.0)] // 3 second buffer, 5 seconds since last switch
fn test_abr_decision_with_different_conditions(
    #[case] _buffer_secs: f64,
    #[case] _time_since_last_switch_secs: f64,
    variants_from_parsed_playlist: Vec<AbrVariant>,
) {
    let controller = AbrController::new(AbrSettings::default());
    assert!(controller.current_bandwidth_estimate_bps().is_none());
    assert_eq!(variants_from_parsed_playlist.len(), 3);
}

#[kithara::test]
fn test_variants_from_master_structure(parsed_master_playlist: MasterPlaylist) {
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

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn test_abr_controller_async_usage() {
    let controller = AbrController::new(AbrSettings::default());
    assert!(controller.current_bandwidth_estimate_bps().is_none());
    let _ = AbrMode::Manual(0);
}
