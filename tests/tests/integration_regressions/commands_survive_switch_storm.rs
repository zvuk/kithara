#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;

use kithara::{
    events::{AudioEvent, DownloaderEvent, Event, QueueEvent, RequestId, RequestMethod},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    BehaviorHandle, Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir,
    audio_fixture::EmbeddedAudio,
    kithara,
    offline::OfflineSession,
    temp_dir,
    waits::{wait_for_event, wait_for_loader_done_event},
};

const TRACK_COUNT: usize = 3;
const STORM_ROUNDS: usize = 12;
const SEEK_TARGET_SECS: f64 = 5.0;
const SEEK_TOLERANCE_SECS: f64 = 1.0;
/// How far playback must carry past the seek landing to prove `play` was not
/// swallowed. A positive fact, so no window of "nothing happened" is needed.
const MIN_RESUME_PROGRESS_SECS: f64 = 1.0;

fn spawn_ticker(queue: Arc<Queue>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            time::sleep(Duration::from_millis(20)).await;
            if queue.tick().is_err() {
                break;
            }
        }
    })
}

/// `pause` and `play` reach the sink as slot commands, so the snapshot the
/// queue exposes converges a tick later. Waiting for that convergence is the
/// command-took-effect fact; never converging is the reported bug.
async fn wait_for_playing(queue: &Queue, expected: bool, deadline: Duration) -> Result<(), String> {
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
    temp_dir: &TestTempDir,
    index: usize,
) -> ResourceConfig {
    let url = handle.child_url(&format!("storm-{index}.mp3"));
    ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader.clone())
        .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
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
                    bytes: Arc::new(EmbeddedAudio::TEST_MP3_BYTES.to_vec()),
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
    let downloader = Downloader::new(
        DownloaderConfig::builder()
            .client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let ticker = spawn_ticker(Arc::clone(&queue));
    let mut status_rx = queue.subscribe();
    let mut probe_rx = queue.subscribe();

    let ids: Vec<_> = handles
        .iter()
        .enumerate()
        .map(|(index, handle)| {
            queue.append(TrackSource::Config(Box::new(resource_config(
                handle,
                &downloader,
                &temp_dir,
                index,
            ))))
        })
        .collect();
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
