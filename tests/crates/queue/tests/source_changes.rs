#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    decode::TrackMetadata,
    host::HostConfig,
    platform::time::Duration,
    play::{PlayWorker, PlayWorkerConfig, ResourceConfig, ResourcePrep, ResourceSrc},
    queue::{Queue, QueueConfig, QueueEvent, Transition},
};
use kithara_integration_tests::{
    TestServerHelper,
    hls_fixture::create_test_downloader,
    kithara,
    offline::{OfflineQueue, QueueTicker, RENDER_PACE},
    served_mp3,
    waits::wait_for_position_at_least,
};
use kithara_test_utils::{temp_dir, wait_until};
use url::Url;

use crate::bufpool_ext::pools;

#[kithara::test(tokio, flash(false))]
#[case(false)]
#[case(true)]
async fn changing_a_current_source_preserves_identity_position_and_play_state(
    #[future(awt)] served_mp3: (TestServerHelper, Url),
    #[case] playing: bool,
) {
    let (_server, url) = served_mp3;
    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let region = pools();
    let queue = OfflineQueue::paced(
        HostConfig::offline(region.clone()).build(),
        Queue::new(
            QueueConfig::builder()
                .prep(
                    ResourcePrep::builder()
                        .worker(PlayWorker::new(PlayWorkerConfig::builder(region).build()))
                        .build(),
                )
                .store(store.clone())
                .build(),
        ),
        RENDER_PACE,
    )
    .await
    .unwrap();
    let mut ticker = QueueTicker::spawn(queue.control(), Duration::from_millis(10));
    let make_source = |url: Url| {
        ResourceConfig::for_src(ResourceSrc::Url(url))
            .downloader(create_test_downloader())
            .store(store.clone())
            .metadata(TrackMetadata {
                title: Some("Catalogue title".to_owned()),
                ..TrackMetadata::default()
            })
            .build()
    };
    let source = make_source(url.clone());
    let id = queue
        .run(move |queue| {
            let id = queue.append(source).unwrap();
            queue.select(id, Transition::None).unwrap();
            id
        })
        .await;
    wait_for_position_at_least(&queue, 0.2, Duration::from_secs(10))
        .await
        .unwrap();
    queue.run(kithara::queue::QueueControl::pause).await;
    wait_until(Duration::from_secs(2), "the pause is applied", || {
        !queue.control().is_playing()
    })
    .await
    .unwrap();
    queue.run(|queue| queue.seek(0.5)).await.unwrap();
    wait_until(Duration::from_secs(2), "the paused seek lands", || {
        queue.control().position_seconds() == Some(0.5)
    })
    .await
    .unwrap();
    let before = queue.control().position_seconds().unwrap();
    assert_eq!(before, 0.5);
    if playing {
        queue.run(kithara::queue::QueueControl::play).await;
        wait_until(Duration::from_secs(2), "the track resumes", || {
            queue.control().is_playing()
        })
        .await
        .unwrap();
    }
    let mut alternate = url;
    alternate.set_query(Some("quality=alternate"));
    let source = make_source(alternate.clone());
    let mut events = queue.subscribe::<QueueEvent>();
    queue
        .run(move |queue| queue.replace_source(id, source))
        .await
        .unwrap();
    wait_until(
        Duration::from_secs(10),
        "the new source enters playback",
        || {
            queue.control().current().is_some_and(|track| {
                track.id == id
                    && !track.source_changing
                    && track.url.as_deref() == Some(alternate.as_str())
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(queue.control().tracks().len(), 1);
    let current = queue.control().current().unwrap();
    assert_eq!(current.id, id);
    assert_eq!(current.metadata().title.as_deref(), Some("Catalogue title"));
    assert_eq!(queue.control().is_playing(), playing);
    let after = queue.control().position_seconds().unwrap();
    if playing {
        assert!(
            after >= before,
            "the playing position moves forward: {before} -> {after}"
        );
        wait_for_position_at_least(&queue, after + 0.1, Duration::from_secs(10))
            .await
            .unwrap();
    } else {
        assert_eq!(after, before, "a paused source retains its exact position");
    }
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(
            event.event,
            QueueEvent::TrackAdded { .. }
                | QueueEvent::TrackRemoved { .. }
                | QueueEvent::CurrentTrackChanged { .. }
        ));
    }
    ticker.stop().await;
    queue.close().await;
}

/// A source change while every mixer slot sounds (the new current fading in,
/// its predecessor fading out) must still land on the current track.
#[kithara::test(tokio)]
async fn a_source_change_on_a_full_mixer_lands_on_the_current_track(
    #[future(awt)] served_mp3: (TestServerHelper, Url),
) {
    use kithara::{
        play::{CrossfadeSettings, DeckMixerConfig},
        queue::QueueSettings,
    };

    let (_server, url) = served_mp3;
    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let region = pools();
    let queue = OfflineQueue::paced(
        HostConfig::offline(region.clone()).build(),
        Queue::new(
            QueueConfig::builder()
                .prep(
                    ResourcePrep::builder()
                        .worker(PlayWorker::new(PlayWorkerConfig::builder(region).build()))
                        .build(),
                )
                .store(store.clone())
                .mixer(
                    DeckMixerConfig::builder()
                        .slots(std::num::NonZeroUsize::new(2).expect("two slots"))
                        .build(),
                )
                .settings(
                    QueueSettings::builder()
                        .crossfade(CrossfadeSettings {
                            duration: 8.0,
                            ..Default::default()
                        })
                        .build(),
                )
                .build(),
        ),
        RENDER_PACE,
    )
    .await
    .unwrap();
    let mut ticker = QueueTicker::spawn(queue.control(), Duration::from_millis(10));
    let make_source = |query: &str| {
        let mut url = url.clone();
        url.set_query(Some(query));
        let source = ResourceConfig::for_src(ResourceSrc::Url(url.clone()))
            .downloader(create_test_downloader())
            .store(store.clone())
            .build();
        (url, source)
    };
    let (_, first) = make_source("item=first");
    let (_, second) = make_source("item=second");
    let (first, second) = queue
        .run(move |queue| {
            let first = queue.append(first).unwrap();
            let second = queue.append(second).unwrap();
            queue.select(first, Transition::None).unwrap();
            (first, second)
        })
        .await;
    wait_for_position_at_least(&queue, 0.2, Duration::from_secs(10))
        .await
        .unwrap();
    let mut events = queue.subscribe::<QueueEvent>();
    queue
        .run(move |queue| queue.select(second, Transition::Crossfade))
        .await
        .unwrap();
    wait_until(
        Duration::from_secs(10),
        "the crossfade fills the mixer",
        || {
            while let Ok(event) = events.try_recv() {
                if matches!(event.event, QueueEvent::CrossfadeStarted { .. }) {
                    return true;
                }
            }
            false
        },
    )
    .await
    .unwrap();
    assert_eq!(
        queue.control().current().map(|track| track.id),
        Some(second)
    );
    assert_ne!(first, second);

    let (alternate, source) = make_source("item=second&quality=alternate");
    queue
        .run(move |queue| queue.replace_source(second, source))
        .await
        .unwrap();
    wait_until(Duration::from_secs(10), "the source change settles", || {
        queue
            .control()
            .current()
            .is_some_and(|track| !track.source_changing)
    })
    .await
    .unwrap();
    let current = queue.control().current().unwrap();
    assert_eq!(current.id, second);
    assert_eq!(
        current.url.as_deref(),
        Some(alternate.as_str()),
        "the full mixer refused the source change: {current:?}"
    );
    ticker.stop().await;
    queue.close().await;
}
