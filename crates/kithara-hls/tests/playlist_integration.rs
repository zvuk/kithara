mod fixture;
use fixture::*;
use kithara_hls::{HlsResult, playlist::PlaylistManager};

#[tokio::test]
async fn fetch_master_playlist_from_network() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (assets, net) = create_test_cache_and_net();
    let assets = assets.assets().clone();

    let playlist_manager = PlaylistManager::new(assets, net, None);
    let master_url = server.url("/master.m3u8")?;

    let master_playlist = playlist_manager.fetch_master_playlist(&master_url).await?;

    assert_eq!(master_playlist.variant_streams.len(), 3);
    Ok(())
}

#[tokio::test]
async fn fetch_media_playlist_from_network() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (assets, net) = create_test_cache_and_net();
    let assets = assets.assets().clone();

    let playlist_manager = PlaylistManager::new(assets, net, None);
    let media_url = server.url("/video/480p/playlist.m3u8")?;

    let media_playlist = playlist_manager.fetch_media_playlist(&media_url).await?;

    let segment_count = media_playlist.segments.into_iter().count();
    assert_eq!(segment_count, 3);
    Ok(())
}

#[tokio::test]
async fn resolve_url_with_base_override() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (assets, net) = create_test_cache_and_net();
    let assets = assets.assets().clone();

    let base_url = server.url("/custom/base/")?;
    let playlist_manager = PlaylistManager::new(assets, net, Some(base_url.clone()));

    let relative_url = "video/480p/playlist.m3u8";
    let resolved = playlist_manager.resolve_url(&base_url, relative_url)?;

    assert!(
        resolved
            .as_str()
            .contains("/custom/base/video/480p/playlist.m3u8"),
        "Expected resolved URL to contain base path"
    );
    Ok(())
}
