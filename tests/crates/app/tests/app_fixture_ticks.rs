#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use std::fs;

use kithara::{
    abr::AbrMode,
    decode::DecoderBackend,
    platform::{time::Duration, tokio::task::spawn_blocking},
    queue::Transition,
};
use kithara_app::document::Config;
use kithara_integration_tests::{
    TestServerHelper, kithara,
    offline::{app_queue, app_track_source},
    served_mp3,
    waits::wait_for_position_at_least,
};
use kithara_test_utils::temp_dir;
use url::Url;

/// The native ticker uses a real timed channel receive, so its lifetime needs a real clock.
#[kithara::test(tokio, flash(false), timeout(Short))]
async fn app_fixture_updates_position_without_manual_ticks(
    #[future(awt)] served_mp3: (TestServerHelper, Url),
) {
    let (_server, url) = served_mp3;
    let document = local_document().await;
    let fixture = app_queue(document).await;
    let source = app_track_source(
        url.as_str(),
        &fixture.config,
        fixture.config.store.clone(),
        DecoderBackend::Symphonia,
        AbrMode::Auto(None),
        None,
    );
    fixture
        .queue
        .run(move |queue| {
            let id = queue.append(source).expect("append local track");
            queue.select(id, Transition::None).expect("select track");
            queue.play();
        })
        .await;

    let advanced = wait_for_position_at_least(&fixture.queue, 0.1, Duration::from_secs(10)).await;
    fixture.close().await;
    advanced.expect("the fixture ticker must publish playback progress");
}

async fn local_document() -> Config {
    spawn_blocking(|| {
        let temp = temp_dir();
        let path = temp.path().join("app.yaml");
        fs::write(&path, "sources:\n  zvuk: null\ndrm:\n  providers: []\n")
            .expect("write local configuration");
        Config::load(Some(&path), None).expect("load configuration without DRM credentials")
    })
    .await
    .expect("load local configuration off the runtime worker")
}

#[kithara::test(tokio, flash(false), timeout(Medium))]
async fn app_fixtures_own_separate_disk_stores() {
    let first = app_queue(local_document().await).await;
    let second = app_queue(local_document().await).await;
    let first_root = first.config.store.root_dir().to_path_buf();
    let second_root = second.config.store.root_dir().to_path_buf();
    let first_cache = first.cache.path().to_path_buf();
    let second_cache = second.cache.path().to_path_buf();
    first.close().await;
    second.close().await;

    assert_eq!(
        first_root, first_cache,
        "the fixture owns its store directory"
    );
    assert_eq!(
        second_root, second_cache,
        "the fixture owns its store directory"
    );
    assert_ne!(
        first_root, second_root,
        "independent fixtures never write the same indices"
    );
}
