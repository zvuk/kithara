mod fixture;
use fixture::*;

#[tokio::test]
#[ignore = "outdated: relied on removed kithara-cache API (CachePath/asset().exists()); will be rewritten for kithara-assets + resource-based storage"]
async fn test_hls_basic_functionality() {
    // Placeholder: keep the test file compiling without asserting old cache behavior.
    // New integration tests will be added once the new assets + storage API is wired into kithara-hls.
    let (_cache, _net) = create_test_cache_and_net();
    unimplemented!("rewrite this test for kithara-assets + resource-based API");
}
