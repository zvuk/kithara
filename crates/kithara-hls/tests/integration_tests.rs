use kithara_hls::fixture::*;

#[tokio::test]
async fn test_hls_basic_functionality() {
    // Just a smoke test to ensure tests compile and run
    let (cache, net) = create_test_cache_and_net();

    // Verify basic creation works
    assert!(
        cache
            .asset(
                crate::AssetId::from_url(&url::Url::parse("http://example.com/test.m3u8").unwrap())
                    .unwrap()
            )
            .exists(&kithara_cache::CachePath::new(vec!["test".to_string()]).unwrap())
            .is_ok()
    );
}
