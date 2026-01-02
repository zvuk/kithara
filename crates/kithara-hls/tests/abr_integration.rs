mod fixture;
use fixture::create_test_master_playlist;
use kithara_hls::HlsResult;
use kithara_hls::abr::{AbrConfig, AbrController};

#[test]
fn test_variant_selection_manual_override() -> HlsResult<()> {
    let config = AbrConfig::default();
    let selector = std::sync::Arc::new(|_playlist: &hls_m3u8::MasterPlaylist| Some(2));
    let mut controller = AbrController::new(config, Some(selector), 0);

    let master_playlist = create_test_master_playlist();
    let selected = controller.select_variant(&master_playlist)?;

    assert_eq!(selected, 2); // Should select manual override
    Ok(())
}
