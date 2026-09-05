#![forbid(unsafe_code)]

use kithara::{
    assets::{AssetResource, ResourceKey},
    hls::{HlsError, HlsResult, VariantId},
    platform::time::Duration,
};
use kithara_integration_tests::{hls_fixture::*, hls_server::*};
use url::Url;

fn browser_timeout(native_secs: u64, wasm_secs: u64) -> Duration {
    if cfg!(target_arch = "wasm32") {
        Duration::from_secs(wasm_secs)
    } else {
        Duration::from_secs(native_secs)
    }
}

fn key_for(assets: &TestAssets, url: &Url) -> ResourceKey {
    assets
        .scope()
        .key(&AssetResource::Url(url.clone()))
        .expect("valid playlist URL")
}

#[kithara::test(tokio, browser, timeout(browser_timeout(5, 30)), hang_timeout_secs(1))]
#[case::v0(0)]
#[case::v1(1)]
async fn fetch_media_playlist_from_network(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
    #[case] variant: usize,
) -> HlsResult<()> {
    let server = test_server.await;
    let fetch_manager = test_playlist_cache(&assets_fixture, net_fixture);
    let media_url = server.url(&format!("/v{variant}.m3u8"));

    let media_playlist = fetch_manager
        .media_playlist(
            &key_for(&assets_fixture, &media_url),
            &media_url,
            VariantId(variant),
        )
        .await?;
    assert_eq!(media_playlist.segments.len(), 3);

    Ok(())
}

#[kithara::test(tokio, browser, timeout(browser_timeout(5, 30)), hang_timeout_secs(1))]
async fn fetch_manager_caching_behavior(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let fetch_manager = test_playlist_cache(&assets_fixture, net_fixture);
    let master_url = server.url("/master.m3u8");

    let master1 = fetch_manager
        .master_playlist(&key_for(&assets_fixture, &master_url), &master_url)
        .await?;
    assert_eq!(master1.variants.len(), 3);

    let master2 = fetch_manager
        .master_playlist(&key_for(&assets_fixture, &master_url), &master_url)
        .await?;
    assert_eq!(master2.variants.len(), 3);

    assert_eq!(master1.variants.len(), master2.variants.len());

    Ok(())
}

#[kithara::test(tokio, browser, timeout(browser_timeout(5, 30)), hang_timeout_secs(1))]
async fn fetch_manager_error_handling_invalid_url(
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let fetch_manager = test_playlist_cache(&assets_fixture, net_fixture);

    let invalid_url = Url::parse("http://127.0.0.1:9/master.m3u8")
        .map_err(|e| HlsError::InvalidUrl(e.to_string()))?;

    let result = fetch_manager
        .master_playlist(&key_for(&assets_fixture, &invalid_url), &invalid_url)
        .await;
    assert!(result.is_err(), "invalid URL should fail, got Ok");

    Ok(())
}

#[kithara::test(tokio, browser, timeout(browser_timeout(5, 30)), hang_timeout_secs(1))]
#[case::file("segment.ts", "/base/segment.ts")]
#[case::current_dir("./segment.ts", "/base/segment.ts")]
#[case::parent_dir("../segment.ts", "/segment.ts")]
#[case::subdir("subdir/segment.ts", "/base/subdir/segment.ts")]
#[case::nested_playlist("video/480p/playlist.m3u8", "/base/video/480p/playlist.m3u8")]
async fn resolve_relative_url(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
    #[case] relative: &str,
    #[case] expected_suffix: &str,
) -> HlsResult<()> {
    let server = test_server.await;
    let base_url = server.url("/base/");
    let fetch_manager = test_playlist_cache(&assets_fixture, net_fixture);
    fetch_manager.set_base_url(Some(base_url.clone()));

    let resolved = fetch_manager.resolve_url(&base_url, relative)?;
    assert!(
        resolved.as_str().ends_with(expected_suffix),
        "Expected {relative} to end with {expected_suffix}, got {resolved}"
    );

    Ok(())
}

#[kithara::test(tokio, browser, timeout(browser_timeout(5, 30)), hang_timeout_secs(1))]
async fn fetch_manager_with_different_base_urls(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let fetch_manager_no_base = test_playlist_cache(&assets_fixture, net_fixture.clone());
    let master_url = server.url("/master.m3u8");
    let master_no_base = fetch_manager_no_base
        .master_playlist(&key_for(&assets_fixture, &master_url), &master_url)
        .await?;
    assert_eq!(master_no_base.variants.len(), 3);

    let base_url = server.url("/custom/base/");
    let fetch_manager_with_base = test_playlist_cache(&assets_fixture, net_fixture);
    fetch_manager_with_base.set_base_url(Some(base_url));

    let master_with_base = fetch_manager_with_base
        .master_playlist(&key_for(&assets_fixture, &master_url), &master_url)
        .await?;
    assert_eq!(master_with_base.variants.len(), 3);

    Ok(())
}
