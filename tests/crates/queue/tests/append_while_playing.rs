#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::AssetStore,
    platform::time::Duration,
    play::{ResourceConfig, ResourceSrc},
    queue::TrackSource,
};
use kithara_integration_tests::{
    TestServerHelper, kithara,
    smoothing::{SmoothingCase, sine_queue},
};
use kithara_test_fixtures::SignalAsset;

use crate::bufpool_ext::pools;

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn queue_append_while_playing_does_not_wait_for_host() {
    let (harness, first_id) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SINE440_60S);
    let src = ResourceSrc::parse(url.as_str()).expect("valid signal fixture URL");
    let second = harness
        .control()
        .append(TrackSource::Config(Box::new(
            ResourceConfig::for_src(src)
                .store(AssetStore::builder(pools()).build())
                .build(),
        )))
        .expect("append while first track is playing");
    assert_ne!(second.as_u64(), first_id);
    harness.close().await;
}
