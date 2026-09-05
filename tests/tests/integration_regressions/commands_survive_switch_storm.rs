#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{AudioEvent, DownloaderEvent, Event, QueueEvent, RequestId, RequestMethod},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    BehaviorHandle, Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir,
    bufpool_ext::{TestPools, pools},
    kithara,
    offline::{OfflineQueue, drive_queue_ticks},
    temp_dir,
    test_defaults::Consts as Shared,
    waits::{wait_for_event, wait_for_loader_done_event},
};
use kithara_test_fixtures::assets::signal_mp3_track_sine440_187s;

const TRACK_COUNT: usize = 3;
const STORM_ROUNDS: usize = 12;
const SEEK_TARGET_SECS: f64 = 5.0;
const SEEK_TOLERANCE_SECS: f64 = 1.0;
/// How far playback must carry past the seek landing to prove `play` was not
/// swallowed. A positive fact, so no window of "nothing happened" is needed.
const MIN_RESUME_PROGRESS_SECS: f64 = 1.0;

/// `pause` and `play` reach the sink as slot commands, so the snapshot the
/// queue exposes converges a tick later. Waiting for that convergence is the
/// command-took-effect fact; never converging is the reported bug.
async fn wait_for_playing(
    queue: &QueueControl<TestPools>,
    expected: bool,
    deadline: Duration,
) -> Result<(), String> {
    time::timeout(deadline, async {
        while queue.playback_view().playing != expected {
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| format!("playback state never became playing={expected} within {deadline:?}"))
}

fn resource_config(
    handle: &BehaviorHandle,
    downloader: &Downloader,
    store: &AssetStore<TestPools>,
    index: usize,
) -> ResourceConfig<TestPools> {
    let url = handle.child_url(&format!("storm-{index}.mp3"));
    ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid fixture URL"))
        .downloader(downloader.clone())
        .store(store.clone())
        .build()
}

fn drain_active_gets(rx: &mut kithara::events::EventReceiver, active: &mut HashSet<RequestId>) {
    while let Ok(envelope) = rx.try_recv() {
        match envelope.event {
            Event::Downloader(DownloaderEvent::RequestEnqueued {
                request_id,
                method: RequestMethod::Get,
                ..
            }) => {
                active.insert(request_id);
            }
            Event::Downloader(DownloaderEvent::RequestCompleted { request_id, .. })
            | Event::Downloader(DownloaderEvent::RequestFailed { request_id, .. })
            | Event::Downloader(DownloaderEvent::RetryExhausted { request_id, .. })
            | Event::Downloader(DownloaderEvent::RequestCancelled { request_id, .. }) => {
                active.remove(&request_id);
            }
            _ => {}
        }
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn commands_still_work_after_a_switch_storm(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let handles: Vec<_> = (0..TRACK_COUNT)
        .map(|_| {
            helper.register_behavior(FixtureBehavior {
                content: Content::StaticBytes {
                    bytes: Arc::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
                    content_type: Some("audio/mpeg"),
                },
                // Throttled so the storm lands while transfers are still in
                // flight — that is the state the report describes.
                delivery: Delivery::Throttle {
                    chunk: 4 * 1024,
                    delay_ms: 20,
                },
            })
        })
        .collect();
    let pools = pools();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            CancelToken::never(),
        ))
        .build(),
    );
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(kithara::play::PlayWorker::new(
                kithara::play::PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::new(
        HostConfig::offline(pools)
            .pacing(Duration::from_millis(10))
            .build(),
        Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .build(),
        ),
    )
    .expect("create product offline queue");
    let ticker = tokio::task::spawn(drive_queue_ticks(
        queue.control(),
        Duration::from_millis(20),
    ));
    let mut status_rx = queue.subscribe();
    let mut probe_rx = queue.subscribe();

    let ids: Vec<_> = handles
        .iter()
        .enumerate()
        .map(|(index, handle)| {
            queue.append(TrackSource::Config(Box::new(resource_config(
                handle,
                &downloader,
                &store,
                index,
            ))))
        })
        .collect::<Result<Vec<_>, _>>()
        .expect("queue is open while fixtures are appended");
    // Only the first track is awaited: the others must still be transferring
    // when the storm starts.
    wait_for_loader_done_event(&mut status_rx, &queue, ids[0], Duration::from_secs(60))
        .await
        .unwrap_or_else(|error| panic!("precondition: first track: {error}"));
    queue.play();

    let mut active_gets = HashSet::new();
    drain_active_gets(&mut probe_rx, &mut active_gets);
    assert!(
        !active_gets.is_empty(),
        "precondition: no throttled GET was in flight when the switch storm began; \
         the reported scenario did not happen"
    );

    for round in 0..STORM_ROUNDS {
        let target = ids[round % ids.len()];
        queue
            .select(target, Transition::None)
            .unwrap_or_else(|error| panic!("switch {round} was rejected: {error}"));
        if queue.current().is_some_and(|entry| entry.id == target) {
            continue;
        }
        wait_for_event(
            &mut status_rx,
            "the switched-to track becoming current",
            |event| {
                matches!(
                    event,
                    Event::Queue(QueueEvent::CurrentTrackChanged { id: Some(id) }) if *id == target
                )
            },
            Duration::from_secs(30),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("switch {round} never made track {target:?} current: {error}")
        });
    }

    queue
        .seek(SEEK_TARGET_SECS)
        .unwrap_or_else(|error| panic!("final seek failed: {error}"));
    let mut landed = 0.0;
    wait_for_event(
        &mut status_rx,
        "the post-storm seek completing",
        |event| {
            let Event::Audio(AudioEvent::SeekComplete { position, .. }) = event else {
                return false;
            };
            landed = position.as_secs_f64();
            true
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| {
        panic!("seek to {SEEK_TARGET_SECS:.1}s never completed after the storm: {error}")
    });
    assert!(
        (landed - SEEK_TARGET_SECS).abs() < SEEK_TOLERANCE_SECS,
        "seek to {SEEK_TARGET_SECS:.1}s landed at {landed:.3}s after the switch storm"
    );

    queue.pause();
    wait_for_playing(&queue, false, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("pause was swallowed after the switch storm: {error}"));
    queue.play();
    wait_for_playing(&queue, true, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("play was swallowed after the switch storm: {error}"));

    let resume_target = landed + MIN_RESUME_PROGRESS_SECS;
    wait_for_event(
        &mut status_rx,
        "playback carrying on after the post-storm pause and play",
        |event| {
            let Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) = event else {
                return false;
            };
            *position_ms as f64 / 1000.0 >= resume_target
        },
        Duration::from_secs(60),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "the queue reports playing after the storm, but the sink never carried \
             past {resume_target:.3}s from the seek landing at {landed:.3}s: {error}"
        )
    });

    queue.clear();
    ticker.abort();
    let _ = ticker.await;
}
