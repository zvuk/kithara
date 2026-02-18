#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use kithara::{
    abr::{AbrController, AbrMode, AbrOptions, AbrReason, Variant},
    hls::parsing::{MasterPlaylist, parse_master_playlist},
};
use rstest::{fixture, rstest};

/// Convert HLS master playlist variants to ABR variant list (test helper).
fn variants_from_master(master: &MasterPlaylist) -> Vec<Variant> {
    master
        .variants
        .iter()
        .map(|v| Variant {
            variant_index: v.id.0,
            bandwidth_bps: v.bandwidth.unwrap_or(0),
        })
        .collect()
}

// Fixtures

#[fixture]
fn abr_config_default() -> AbrOptions {
    AbrOptions::default()
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
fn parsed_master_playlist(test_master_playlist_data: &str) -> MasterPlaylist {
    parse_master_playlist(test_master_playlist_data.as_bytes())
        .expect("Failed to parse master playlist")
}

#[fixture]
fn variants_from_parsed_playlist(parsed_master_playlist: MasterPlaylist) -> Vec<Variant> {
    variants_from_master(&parsed_master_playlist)
}

// Test Cases

#[rstest]
fn test_variant_selection_manual_override(variants_from_parsed_playlist: Vec<Variant>) {
    let opts = AbrOptions {
        mode: AbrMode::Manual(2),
        variants: variants_from_parsed_playlist,
        ..Default::default()
    };
    let controller = AbrController::new(opts);

    let decision = controller.decide(Instant::now());

    assert_eq!(decision.target_variant_index, 2);
    assert_eq!(decision.reason, AbrReason::ManualOverride);
}

#[rstest]
#[case(0)]
#[case(1)]
#[case(2)]
#[case(3)]
fn test_manual_selector_different_indices(
    #[case] selector_index: usize,
    variants_from_parsed_playlist: Vec<Variant>,
) {
    let opts = AbrOptions {
        mode: AbrMode::Manual(selector_index),
        variants: variants_from_parsed_playlist,
        ..Default::default()
    };
    let controller = AbrController::new(opts);

    let decision = controller.decide(Instant::now());

    // Manual mode always returns the configured index
    assert_eq!(decision.target_variant_index, selector_index);
    assert_eq!(decision.reason, AbrReason::ManualOverride);
}

#[rstest]
fn test_abr_controller_no_selector(
    mut abr_config_default: AbrOptions,
    variants_from_parsed_playlist: Vec<Variant>,
) {
    let variants_len = variants_from_parsed_playlist.len();
    abr_config_default.variants = variants_from_parsed_playlist;
    let controller = AbrController::new(abr_config_default);

    let decision = controller.decide(Instant::now());

    // Without manual mode, should use default ABR logic
    assert!(decision.target_variant_index < variants_len);
    assert_ne!(decision.reason, AbrReason::ManualOverride);
}

#[rstest]
#[case(0.0, 0.0)] // No buffer
#[case(1.0, 0.0)] // 1 second buffer
#[case(5.0, 0.0)] // 5 second buffer
#[case(0.0, 2.0)] // No buffer, 2 seconds since last switch
#[case(3.0, 5.0)] // 3 second buffer, 5 seconds since last switch
fn test_abr_decision_with_different_conditions(
    #[case] _buffer_secs: f64,
    #[case] _time_since_last_switch_secs: f64,
    variants_from_parsed_playlist: Vec<Variant>,
) {
    let opts = AbrOptions {
        mode: AbrMode::Manual(1),
        variants: variants_from_parsed_playlist,
        ..Default::default()
    };
    let controller = AbrController::new(opts);

    let decision = controller.decide(Instant::now());

    // Should still respect manual override regardless of conditions
    assert_eq!(decision.target_variant_index, 1);
    assert_eq!(decision.reason, AbrReason::ManualOverride);
}

#[rstest]
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

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_abr_controller_async_usage() {
    // Test that ABR controller can be used in async context
    let config = AbrOptions {
        mode: AbrMode::Manual(0),
        ..Default::default()
    };
    let _controller = AbrController::new(config);

    // Verify the controller was created successfully in async context
    assert_eq!(_controller.decide(Instant::now()).target_variant_index, 0);
}
