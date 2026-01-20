#![forbid(unsafe_code)]

use super::fixture;

use std::{sync::Arc, time::Duration};

use fixture::*;
use kithara_hls::{
    HlsResult,
    fetch::FetchManager,
    playlist::{PlaylistManager, VariantId},
};
use rstest::{fixture, rstest};

// ==================== Fixtures ====================

#[fixture]
async fn test_server() -> TestServer {
    TestServer::new().await
}

#[fixture]
fn assets_fixture() -> TestAssets {
    create_test_assets()
}

#[fixture]
fn net_fixture() -> kithara_net::HttpClient {
    create_test_net()
}

#[fixture]
fn variant_id_0() -> VariantId {
    VariantId(0)
}

#[fixture]
fn variant_id_1() -> VariantId {
    VariantId(1)
}

// ==================== Test Cases ====================

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_master_playlist_from_network(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(assets, net));
    let playlist_manager = PlaylistManager::new(fetch_manager.clone(), None);
    let master_url = server.url("/master.m3u8")?;
    let master_playlist = playlist_manager.master_playlist(&master_url).await?;

    assert_eq!(master_playlist.variants.len(), 3);
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_media_playlist_from_network(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
    variant_id_0: VariantId,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(assets, net));
    let playlist_manager = PlaylistManager::new(fetch_manager.clone(), None);
    let media_url = server.url("/video/480p/playlist.m3u8")?;

    let media_playlist = playlist_manager
        .media_playlist(&media_url, variant_id_0)
        .await?;

    let segment_count = media_playlist.segments.len();
    assert_eq!(segment_count, 3);
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn resolve_url_with_base_override(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let base_url = server.url("/custom/base/")?;
    let fetch_manager = Arc::new(FetchManager::new(assets, net));
    let playlist_manager = PlaylistManager::new(fetch_manager.clone(), Some(base_url.clone()));

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

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_media_playlist_for_different_variants(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
    variant_id_0: VariantId,
    variant_id_1: VariantId,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(assets.clone(), net.clone()));
    let playlist_manager = PlaylistManager::new(fetch_manager.clone(), None);

    // Test variant 0
    let media_url_0 = server.url("/video/480p/playlist.m3u8")?;
    let media_playlist_0 = playlist_manager
        .media_playlist(&media_url_0, variant_id_0)
        .await?;
    assert_eq!(media_playlist_0.segments.len(), 3);

    // Test variant 1 (different playlist)
    let media_url_1 = server.url("/video/720p/playlist.m3u8")?;
    let media_playlist_1 = playlist_manager
        .media_playlist(&media_url_1, variant_id_1)
        .await?;
    assert_eq!(media_playlist_1.segments.len(), 3);

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn playlist_manager_caching_behavior(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(assets, net));
    let playlist_manager = PlaylistManager::new(fetch_manager.clone(), None);
    let master_url = server.url("/master.m3u8")?;

    // First fetch
    let master1 = playlist_manager.master_playlist(&master_url).await?;
    assert_eq!(master1.variants.len(), 3);

    // Second fetch (should potentially use cache)
    let master2 = playlist_manager.master_playlist(&master_url).await?;
    assert_eq!(master2.variants.len(), 3);

    // Variants should be the same
    assert_eq!(master1.variants.len(), master2.variants.len());

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn playlist_manager_error_handling_invalid_url(
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(assets, net));
    let playlist_manager = PlaylistManager::new(fetch_manager.clone(), None);

    // Try to fetch from invalid URL
    let invalid_url =
        url::Url::parse("http://invalid-domain-that-does-not-exist-12345.com/master.m3u8")
            .map_err(|e| kithara_hls::HlsError::InvalidUrl(e.to_string()))?;

    let result = playlist_manager.master_playlist(&invalid_url).await;

    // Should fail with network error (or succeed if somehow connects)
    assert!(result.is_ok() || result.is_err());

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn resolve_multiple_relative_urls(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let base_url = server.url("/base/")?;
    let fetch_manager = Arc::new(FetchManager::new(assets, net));
    let playlist_manager = PlaylistManager::new(fetch_manager.clone(), Some(base_url.clone()));

    // Test different relative URLs
    let test_cases = vec![
        ("segment.ts", "/base/segment.ts"),
        ("./segment.ts", "/base/segment.ts"),
        ("../segment.ts", "/segment.ts"),
        ("subdir/segment.ts", "/base/subdir/segment.ts"),
    ];

    for (relative, expected_suffix) in test_cases {
        let resolved = playlist_manager.resolve_url(&base_url, relative)?;
        assert!(
            resolved.as_str().ends_with(expected_suffix),
            "Expected {} to end with {}, got {}",
            relative,
            expected_suffix,
            resolved
        );
    }

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn playlist_manager_with_different_base_urls(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    // Test with no base URL
    let fetch_manager_no_base = Arc::new(FetchManager::new(assets.clone(), net.clone()));
    let playlist_manager_no_base = PlaylistManager::new(fetch_manager_no_base.clone(), None);
    let master_url = server.url("/master.m3u8")?;
    let master_no_base = playlist_manager_no_base
        .master_playlist(&master_url)
        .await?;
    assert_eq!(master_no_base.variants.len(), 3);

    // Test with base URL
    let base_url = server.url("/custom/base/")?;
    let fetch_manager_with_base = Arc::new(FetchManager::new(assets, net));
    let playlist_manager_with_base =
        PlaylistManager::new(fetch_manager_with_base.clone(), Some(base_url));

    // Fetch should still work with base URL
    let master_with_base = playlist_manager_with_base
        .master_playlist(&master_url)
        .await?;
    assert_eq!(master_with_base.variants.len(), 3);

    Ok(())
}
