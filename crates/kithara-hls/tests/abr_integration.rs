mod fixture;
use std::time::Instant;

use fixture::create_test_master_playlist;
use kithara_hls::{
    abr::{AbrConfig, AbrController, AbrReason, variants_from_master},
    playlist::parse_master_playlist,
};

#[test]
fn test_variant_selection_manual_override() {
    let config = AbrConfig::default();
    let selector = std::sync::Arc::new(|_playlist: &kithara_hls::playlist::MasterPlaylist| Some(2));
    let controller = AbrController::new(config, Some(selector), 0);

    let hls_m3u8_master = create_test_master_playlist();
    let playlist_str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID="vid",NAME="720p",BANDWIDTH=2000000,RESOLUTION=1280x720
#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720,CODECS="avc1.64001f,mp4a.40.2"
video/720p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=854x480,CODECS="avc1.64001f,mp4a.40.2"
video/480p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=500000,RESOLUTION=640x360,CODECS="avc1.64001f,mp4a.40.2"
video/360p/playlist.m3u8
"#;
    let master_playlist =
        parse_master_playlist(playlist_str.as_bytes()).expect("Failed to parse master playlist");
    let variants = variants_from_master(&master_playlist);
    let decision = controller.decide_for_master(&master_playlist, &variants, 0.0, Instant::now());

    assert_eq!(decision.target_variant_index, 2);
    assert_eq!(decision.reason, AbrReason::ManualOverride);
}
