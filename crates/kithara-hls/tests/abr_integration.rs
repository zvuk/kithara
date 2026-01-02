mod fixture;
use fixture::create_test_master_playlist;
use kithara_hls::abr::{AbrConfig, AbrController, AbrReason, variants_from_master};
use std::time::Instant;

#[test]
fn test_variant_selection_manual_override() {
    let config = AbrConfig::default();
    let selector = std::sync::Arc::new(|_playlist: &hls_m3u8::MasterPlaylist| Some(2));
    let controller = AbrController::new(config, Some(selector), 0);

    let master_playlist = create_test_master_playlist();
    let variants = variants_from_master(&master_playlist);
    let decision = controller.decide_for_master(&master_playlist, &variants, 0.0, Instant::now());

    assert_eq!(decision.target_variant_index, 2);
    assert_eq!(decision.reason, AbrReason::ManualOverride);
}
