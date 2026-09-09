#![cfg(not(target_arch = "wasm32"))]

use std::fs;

use kithara::{
    decode::DecoderBackend,
    events::AbrMode,
    platform::{time::Duration, tokio::task::spawn_blocking},
    queue::Transition,
};
use kithara_app::document::Config;
use kithara_integration_tests::{
    TestServerHelper, kithara, offline::app_queue, served_mp3, temp_dir,
    waits::wait_for_position_at_least,
};
use url::Url;

use super::{app_disk_asset_store, app_track_source};

/// The native ticker uses a real timed channel receive, so its lifetime needs a real clock.
#[kithara::test(tokio, flash(false))]
async fn app_fixture_updates_position_without_manual_ticks(
    #[future(awt)] served_mp3: (TestServerHelper, Url),
) {
    let (_server, url) = served_mp3;
    let document = spawn_blocking(|| {
        let temp = temp_dir();
        let path = temp.path().join("app.yaml");
        fs::write(&path, "drm:\n  providers: []\n").expect("write local configuration");
        Config::load(Some(&path), None).expect("load configuration without DRM credentials")
    })
    .await
    .expect("load local configuration off the runtime worker");
    let fixture = app_queue(document).await;
    let source = app_track_source(
        url.as_str(),
        &fixture.config,
        app_disk_asset_store(&fixture.config, fixture.cache.path()),
        DecoderBackend::Symphonia,
        AbrMode::Auto(None),
        None,
    );
    fixture
        .queue
        .run(move |queue| {
            let id = queue.append(source).expect("append local track");
            queue.select(id, Transition::None).expect("select track");
        })
        .await;

    let advanced = wait_for_position_at_least(&fixture.queue, 0.1, Duration::from_secs(10)).await;
    fixture.close().await;
    advanced.expect("the fixture ticker must publish playback progress");
}
