#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use std::fs;

use axum::{Router, http::HeaderMap, routing::get};
use kithara::{
    abr::AbrMode,
    decode::DecoderBackend,
    download::{Downloader, DownloaderConfig},
    net::{HttpClient, NetOptions},
    platform::{time::Duration, tokio::task::spawn_blocking},
    queue::Transition,
};
use kithara_app::{catalog::Catalog, document::Config};
use kithara_app_library::{Playable, PlaybackChoice};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::PcmPattern,
    kithara,
    offline::{AppQueueFixture, LOCAL_LOAD_DEADLINE, app_queue, app_track_source},
    served_mp3,
    waits::wait_for_position_at_least,
};
use kithara_test_fixtures::SignalAsset;
use kithara_test_utils::{TestHttpServer, cancel_token, temp_dir, wait_until};
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
        fs::write(&path, "sources:\n  zvuk: null\ndrm:\n  providers:\n    - name: local\n      domains: [127.0.0.1]\n      cipher_key: fixture\n      headers: {X-Quality: adaptive}\n    - name: fixed\n      domains: [localhost]\n      cipher_key: fixture\n      headers: {X-Quality: progressive}\n")
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
async fn source_startup_quality_yields_to_each_deck_and_existing_resources(
    #[future(awt)] served_mp3: (TestServerHelper, Url),
) {
    let (server, mp3) = served_mp3;
    let body = server
        .modeled_client(NetOptions::default(), cancel_token())
        .get_bytes(mp3, None)
        .await
        .unwrap();
    let guarded = TestHttpServer::new(Router::new().route(
        "/track/streamhq",
        get(move |headers: HeaderMap| {
            let body = body.clone();
            async move {
                assert_eq!(headers.get("X-Quality").unwrap(), "progressive");
                body
            }
        }),
    ))
    .await;
    let mut mp3 = guarded.url("/track/streamhq");
    mp3.set_host(Some("localhost")).unwrap();
    mp3.set_query(Some("id=fixture&quality=high"));
    let flac = server.signal(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT);
    let media = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(3)
                .segments_per_variant(4)
                .segment_duration_secs(2.0)
                .packaged_audio_per_variant_pcm_flac(44_100, 2, vec![PcmPattern::Ascending; 3]),
        )
        .await
        .unwrap();
    let hls = media.master_url();
    let choices = vec![
        PlaybackChoice {
            source: hls.clone(),
            label: "AUTO".to_owned(),
            adaptive: true,
        },
        PlaybackChoice {
            source: mp3.clone(),
            label: "MP3 High".to_owned(),
            adaptive: false,
        },
        PlaybackChoice {
            source: flac.clone(),
            label: "FLAC".to_owned(),
            adaptive: false,
        },
    ];
    let mut track = Playable::new(mp3.to_string());
    track.identity = Some("zvuk:track:fixture".to_owned());
    track.choices = choices;
    track.title = Some("Catalogue track".to_owned());
    let mut fixed = app_queue(local_document().await).await;
    let mut automatic = app_queue(local_document().await).await;
    for fixture in [&mut fixed, &mut automatic] {
        let client = HttpClient::new(
            NetOptions::builder().is_insecure(true).build(),
            fixture.config.worker.pools().clone(),
            fixture.config.shutdown.child(),
        );
        fixture.config.downloader = Downloader::new(
            DownloaderConfig::for_client(client)
                .runtime(kithara::platform::tokio::runtime::Handle::current())
                .build(),
        );
    }
    let mut catalogues = Vec::new();
    for (fixture, source) in [(&fixed, &mp3), (&automatic, &hls)] {
        let mut initial = track.clone();
        initial.source = source.to_string();
        let config = fixture.config.clone();
        let (catalogue, result) = fixture
            .queue
            .run(move |queue| {
                let mut catalogue = Catalog::default();
                let result = catalogue.load_onto(queue, &initial, &config);
                (catalogue, result)
            })
            .await;
        result.unwrap();
        wait_source(fixture, source).await;
        catalogues.push(catalogue);
    }
    let mut auto_catalogue = catalogues.pop().unwrap();
    let mut fixed_catalogue = catalogues.pop().unwrap();
    assert_eq!(
        automatic
            .queue
            .control()
            .current_abr_handle()
            .unwrap()
            .mode(),
        Some(AbrMode::Auto(None))
    );
    let id = fixed.queue.control().current().unwrap().id;
    fixed_catalogue = choose(&fixed, fixed_catalogue, None).await;
    wait_source(&fixed, &hls).await;
    auto_catalogue = choose(&automatic, auto_catalogue, Some(1)).await;
    wait_source(&automatic, &mp3).await;
    fixed.queue.run(kithara::queue::QueueControl::pause).await;
    wait_until(LOCAL_LOAD_DEADLINE, "pause applies", || {
        !fixed.queue.control().is_playing()
    })
    .await
    .unwrap();
    let position = fixed.queue.control().position_seconds();
    fixed_catalogue = choose(&fixed, fixed_catalogue, Some(2)).await;
    wait_source(&fixed, &flac).await;
    let current = fixed.queue.control().current().unwrap();
    assert_eq!(current.id, id);
    assert_eq!(fixed.queue.control().position_seconds(), position);
    assert!(!fixed.queue.control().is_playing());
    assert_eq!(current.metadata().title.as_deref(), Some("Catalogue track"));
    let mut repeated = track;
    repeated.source = hls.to_string();
    let config = fixed.config.clone();
    fixed
        .queue
        .run(move |queue| fixed_catalogue.load_onto(queue, &repeated, &config))
        .await
        .unwrap();
    assert_eq!(fixed.queue.control().tracks().len(), 1);
    assert_eq!(
        fixed.queue.control().current().unwrap().url.as_deref(),
        Some(flac.as_str())
    );
    assert_eq!(
        automatic.queue.control().current().unwrap().url.as_deref(),
        Some(mp3.as_str())
    );
    drop(auto_catalogue);
    automatic.close().await;
    fixed.close().await;
}

async fn choose(fixture: &AppQueueFixture, catalogue: Catalog, variant: Option<usize>) -> Catalog {
    let id = fixture
        .queue
        .control()
        .current()
        .expect("a loaded track")
        .id;
    let config = fixture.config.clone();
    let (catalogue, result) = fixture
        .queue
        .run(move |queue| {
            assert!(
                catalogue
                    .select_quality(queue, id, Some(usize::MAX), &config)
                    .is_err()
            );
            let result = catalogue.select_quality(queue, id, variant, &config);
            (catalogue, result)
        })
        .await;
    result.expect("the available quality is accepted");
    catalogue
}

async fn wait_source(fixture: &AppQueueFixture, source: &Url) {
    wait_until(
        LOCAL_LOAD_DEADLINE,
        "selected source enters playback",
        || {
            fixture.queue.control().current().is_some_and(|track| {
                !track.source_changing && track.url.as_deref() == Some(source.as_str())
            })
        },
    )
    .await
    .expect("the selected source is accepted");
}
