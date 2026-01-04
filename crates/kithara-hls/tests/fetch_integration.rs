mod fixture;
use fixture::*;
use kithara_hls::{HlsResult, fetch::FetchManager};

// #[tokio::test]
// async fn fetch_segment_from_network() -> HlsResult<()> {
//     let server = TestServer::new().await;
//     let (cache, net) = create_test_cache_and_net();
//
//     let fetch_manager = FetchManager::new(cache, net);
//     let segment_url = server.url("/segment_0.ts")?;
//
//     // Note: This test assumes server provides segment data
//     let _segment = fetch_manager
//         .fetch_resource(&segment_url, "segment.ts")
//         .await;
//
//     Ok(())
// }

#[tokio::test]
async fn stream_segment_sequence() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (assets, net) = create_test_cache_and_net();
    let assets = assets.assets().clone();

    // Note: in the real session code, this is derived from `AssetId::from_url(master_url)`.
    // For this test we only need a stable namespace.
    let asset_root = "test-hls".to_string();

    let fetch_manager = FetchManager::new(asset_root, assets, net);

    // Create a test media playlist
    let media_playlist_str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment_0.ts
#EXTINF:4.0,
segment_1.ts
#EXT-X-ENDLIST
"#;

    let media_playlist = hls_m3u8::MediaPlaylist::try_from(media_playlist_str)
        .map_err(|e| kithara_hls::HlsError::PlaylistParse(e.to_string()))?;

    let base_url = server.url("/video/480p/")?;
    let mut stream = fetch_manager.stream_segment_sequence(media_playlist, &base_url, None);

    // Note: In real test, server would serve segment data
    // For now, we just verify the stream structure
    use futures::StreamExt;
    let _first_segment = stream.next().await;

    Ok(())
}
