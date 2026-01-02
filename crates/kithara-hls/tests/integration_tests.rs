mod fixture;
use fixture::*;

#[tokio::test]
async fn test_hls_basic_functionality() {
    // Just a smoke test to ensure tests compile and run
    let (cache, net) = create_test_cache_and_net();

    // Verify cache is accessible (empty cache should return false for non-existent path)
    let url = url::Url::parse("http://example.com/test.m3u8").unwrap();
    let asset_id = kithara_core::AssetId::from_url(&url).unwrap();
    let path = kithara_cache::CachePath::new(vec!["test".to_string()]).unwrap();
    let exists = cache.asset(asset_id).exists(&path);
    // Cache is empty, so exists should be false
    assert!(!exists);
}
