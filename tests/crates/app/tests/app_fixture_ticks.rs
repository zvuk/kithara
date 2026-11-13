#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use std::fs;

use kithara::{
    abr::AbrMode,
    decode::DecoderBackend,
    platform::{time::Duration, tokio::task::spawn_blocking},
    queue::Transition,
};
use kithara_app::{catalog::load_onto, document::Config};
use kithara_app_library::Playable;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::PcmPattern,
    kithara,
    offline::{LOCAL_LOAD_DEADLINE, app_queue, app_track_source},
    served_mp3,
    waits::wait_for_position_at_least,
};
use kithara_test_utils::{temp_dir, wait_until};
use url::Url;

/// The native ticker uses a real timed channel receive, so its lifetime needs a real clock.
#[kithara::test(tokio, flash(false))]
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

#[kithara::test(tokio, flash(false))]
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

#[kithara::test(tokio, flash(false))]
async fn source_startup_quality_yields_to_each_deck_and_existing_resources() {
    let server = TestServerHelper::new().await;
    let media = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(3)
                .segments_per_variant(4)
                .segment_duration_secs(2.0)
                .packaged_audio_per_variant_pcm_flac(44_100, 2, vec![PcmPattern::Ascending; 3]),
        )
        .await
        .expect("multiple playable HLS variants");
    let fixed = app_queue(local_document().await).await;
    let automatic = app_queue(local_document().await).await;
    for (fixture, adaptive) in [(&fixed, false), (&automatic, true)] {
        let mut track = Playable::new(media.master_url().to_string());
        track.adaptive_streaming = Some(adaptive);
        let config = fixture.config.clone();
        fixture
            .queue
            .run(move |queue| load_onto(queue, &track, &config))
            .await
            .expect("the source track loads");
        wait_until(LOCAL_LOAD_DEADLINE, "the HLS ladder is ready", || {
            fixture
                .queue
                .control()
                .current_abr_handle()
                .is_some_and(|handle| handle.variants().len() == 3)
        })
        .await
        .expect("the resource exposes its HLS variants");
    }
    let fixed_handle = fixed.queue.control().current_abr_handle().unwrap();
    let auto_handle = automatic.queue.control().current_abr_handle().unwrap();
    assert_eq!(fixed_handle.mode(), Some(AbrMode::manual(0)));
    assert_eq!(auto_handle.mode(), Some(AbrMode::Auto(None)));
    fixed_handle
        .set_mode(AbrMode::Auto(None))
        .expect("deck AUTO overrides fixed startup");
    assert_eq!(fixed_handle.mode(), Some(AbrMode::Auto(None)));
    fixed_handle
        .set_mode(AbrMode::manual(1))
        .expect("deck quality overrides source default");
    let mut repeated = Playable::new(media.master_url().to_string());
    repeated.adaptive_streaming = Some(true);
    let config = fixed.config.clone();
    fixed
        .queue
        .run(move |queue| load_onto(queue, &repeated, &config))
        .await
        .expect("the queued track is selected again");
    assert_eq!(fixed.queue.control().tracks().len(), 1);
    assert_eq!(
        fixed.queue.control().current_abr_handle().unwrap().mode(),
        Some(AbrMode::manual(1))
    );
    assert_eq!(auto_handle.mode(), Some(AbrMode::Auto(None)));
    automatic.close().await;
    fixed.close().await;
}
