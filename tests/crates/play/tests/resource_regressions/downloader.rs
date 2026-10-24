#![cfg(not(target_arch = "wasm32"))]

use kithara::hls::AbrMode;
use kithara_integration_tests::hls_server::packaged_ladder;

use super::*;

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(2)))]
async fn a_remote_hls_resource_without_a_downloader_is_refused() {
    let server = TestServerHelper::new().await;
    let hls = server
        .create_hls(packaged_ladder())
        .await
        .expect("create remote HLS fixture");
    let region = pools();
    let config = ResourceConfig::for_src(ResourceSrc::Url(hls.master_url()))
        .store(AssetStore::builder(region.clone()).build())
        .worker(play_worker(&region))
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let Err(error) = kithara_integration_tests::mock::open_resource(&config).await else {
        panic!("a remote HLS resource opened without a Downloader");
    };
    assert!(
        error.to_string().contains("requires a Downloader"),
        "{error}"
    );
}
