mod fixture;
use std::time::Duration;

use fixture::*;
use rstest::rstest;

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
#[ignore = "rewrite this test for kithara-assets + resource-based API"]
async fn test_hls_basic_functionality() {
    // Placeholder: keep the test file compiling without asserting old cache behavior.
    // New integration tests will be added once the new assets + storage API is wired into kithara-hls.
    let (_cache, _net) = create_test_cache_and_net();
    unimplemented!("rewrite this test for kithara-assets + resource-based API");
}
