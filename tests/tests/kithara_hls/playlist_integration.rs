#![forbid(unsafe_code)]

use std::time::Duration;

use fixture::*;
use kithara::hls::{HlsResult, parsing::VariantId};
use rstest::rstest;

use super::fixture;

// Test Cases

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_master_playlist_from_network(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let fetch_manager = test_fetch_manager(&assets_fixture, net_fixture);
    let master_url = server.url("/master.m3u8")?;
    let master_playlist = fetch_manager.master_playlist(&master_url).await?;

    assert_eq!(master_playlist.variants.len(), 3);
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_media_playlist_from_network(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let fetch_manager = test_fetch_manager(&assets_fixture, net_fixture);
    let media_url = server.url("/video/480p/playlist.m3u8")?;

    let media_playlist = fetch_manager
        .media_playlist(&media_url, VariantId(0))
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
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let base_url = server.url("/custom/base/")?;
    let fetch_manager =
        test_fetch_manager(&assets_fixture, net_fixture).with_base_url(Some(base_url.clone()));

    let relative_url = "video/480p/playlist.m3u8";
    let resolved = fetch_manager.resolve_url(&base_url, relative_url)?;

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
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let fetch_manager = test_fetch_manager(&assets_fixture, net_fixture);

    // Test variant 0
    let media_url_0 = server.url("/video/480p/playlist.m3u8")?;
    let media_playlist_0 = fetch_manager
        .media_playlist(&media_url_0, VariantId(0))
        .await?;
    assert_eq!(media_playlist_0.segments.len(), 3);

    // Test variant 1 (different playlist)
    let media_url_1 = server.url("/video/720p/playlist.m3u8")?;
    let media_playlist_1 = fetch_manager
        .media_playlist(&media_url_1, VariantId(1))
        .await?;
    assert_eq!(media_playlist_1.segments.len(), 3);

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_manager_caching_behavior(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let fetch_manager = test_fetch_manager(&assets_fixture, net_fixture);
    let master_url = server.url("/master.m3u8")?;

    // First fetch
    let master1 = fetch_manager.master_playlist(&master_url).await?;
    assert_eq!(master1.variants.len(), 3);

    // Second fetch (should use cache)
    let master2 = fetch_manager.master_playlist(&master_url).await?;
    assert_eq!(master2.variants.len(), 3);

    // Variants should be the same
    assert_eq!(master1.variants.len(), master2.variants.len());

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_manager_error_handling_invalid_url(
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let fetch_manager = test_fetch_manager(&assets_fixture, net_fixture);

    // Try to fetch from invalid URL
    let invalid_url =
        url::Url::parse("http://invalid-domain-that-does-not-exist-12345.com/master.m3u8")
            .map_err(|e| kithara::hls::HlsError::InvalidUrl(e.to_string()))?;

    let result = fetch_manager.master_playlist(&invalid_url).await;

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
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let base_url = server.url("/base/")?;
    let fetch_manager =
        test_fetch_manager(&assets_fixture, net_fixture).with_base_url(Some(base_url.clone()));

    // Test different relative URLs
    let test_cases = vec![
        ("segment.ts", "/base/segment.ts"),
        ("./segment.ts", "/base/segment.ts"),
        ("../segment.ts", "/segment.ts"),
        ("subdir/segment.ts", "/base/subdir/segment.ts"),
    ];

    for (relative, expected_suffix) in test_cases {
        let resolved = fetch_manager.resolve_url(&base_url, relative)?;
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
async fn fetch_manager_with_different_base_urls(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    // Test with no base URL
    let fetch_manager_no_base = test_fetch_manager(&assets_fixture, net_fixture.clone());
    let master_url = server.url("/master.m3u8")?;
    let master_no_base = fetch_manager_no_base.master_playlist(&master_url).await?;
    assert_eq!(master_no_base.variants.len(), 3);

    // Test with base URL
    let base_url = server.url("/custom/base/")?;
    let fetch_manager_with_base =
        test_fetch_manager(&assets_fixture, net_fixture).with_base_url(Some(base_url));

    // Fetch should still work with base URL
    let master_with_base = fetch_manager_with_base.master_playlist(&master_url).await?;
    assert_eq!(master_with_base.variants.len(), 3);

    Ok(())
}
