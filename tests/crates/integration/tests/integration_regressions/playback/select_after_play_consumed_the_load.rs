#![cfg(not(target_arch = "wasm32"))]

use std::{fs, future::Future, path::PathBuf};

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{EventReceiver, TrackId},
    platform::{
        time::{self, Duration, WallInstant},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{ResourceConfig, ResourceSrc},
    queue::{QueueEvent, TrackSource, Transition},
};
use kithara_integration_tests::{
    bufpool_ext::pools,
    event::TestEvent,
    kithara,
    offline::{OfflinePlayer, OfflinePlayerOptions},
};
use kithara_test_fixtures::fixtures::tone_mp3;
use kithara_test_utils::{TestTempDir, temp_dir};

const TRACK_COUNT: usize = 2;
const BLOCK_FRAMES: usize = 512;

#[kithara::fixture]
fn gated_paths(temp_dir: TestTempDir, tone_mp3: &'static [u8]) -> (TestTempDir, Vec<PathBuf>) {
    let paths = (0..TRACK_COUNT)
        .map(|index| {
            let path = temp_dir.path().join(format!("gated-{index}.mp3"));
            fs::write(&path, tone_mp3).expect("fixture must be writable");
            path
        })
        .collect();
    (temp_dir, paths)
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn completed<Output>(operation: impl Future<Output = Output>) -> Output {
    let mut operation = std::pin::pin!(operation);
    loop {
        hang_tick!();
        if let Ok(output) = time::timeout(Duration::from_millis(10), operation.as_mut()).await {
            hang_reset!();
            return output;
        }
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn wait_for_queue_event(
    harness: &OfflinePlayer,
    events: &mut EventReceiver<TestEvent>,
    label: &str,
    is_awaited: impl Fn(&TestEvent) -> bool,
) {
    let deadline = WallInstant::now() + Duration::from_secs(60);
    loop {
        hang_tick!();
        loop {
            hang_tick!();
            assert!(WallInstant::now() < deadline, "precondition: {label}");
            match events.try_recv() {
                Ok(envelope) if is_awaited(&envelope.event) => {
                    hang_reset!();
                    return;
                }
                Ok(_) | Err(TryRecvError::Lagged(_)) => {
                    hang_reset!();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => panic!("precondition: {label}: event bus closed"),
            }
        }
        assert!(WallInstant::now() < deadline, "precondition: {label}");
        let _ = completed(harness.render(BLOCK_FRAMES)).await;
        time::sleep(Duration::from_millis(5)).await;
    }
}

#[kithara::test(tokio, multi_thread)]
#[kithara_test_utils::kithara::hang_watchdog]
async fn a_track_play_consumed_mid_load_can_be_selected_again(
    gated_paths: (TestTempDir, Vec<PathBuf>),
) {
    let (temp_dir, paths) = gated_paths;
    let store = AssetStore::builder(pools())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let harness = completed(OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        44_100,
    ))
    .await;
    let mut status_rx = harness.player().subscribe();
    let ids = completed(harness.with_queue(move |queue| {
        let ids = paths
            .into_iter()
            .map(|path| {
                queue
                    .append(TrackSource::Config(Box::new(
                        ResourceConfig::for_src(ResourceSrc::Path(path))
                            .store(store.clone())
                            .build(),
                    )))
                    .expect("queue is open while fixtures are appended")
            })
            .collect::<Vec<TrackId>>();
        queue.play();
        ids
    }))
    .await;

    wait_for_queue_event(
        &harness,
        &mut status_rx,
        "the first track's load landing after the owner applied play",
        |event| {
            matches!(
                event,
                TestEvent::Queue(QueueEvent::NextTrackReady { id, .. }) if *id == ids[0]
            )
        },
    )
    .await;

    let second = ids[1];
    completed(harness.with_queue(move |queue| queue.select(second, Transition::None)))
        .await
        .expect("selecting the second track must be accepted");
    wait_for_queue_event(
        &harness,
        &mut status_rx,
        "the second track becoming current",
        |event| {
            matches!(
                event,
                TestEvent::Queue(QueueEvent::CurrentTrackChanged { id: Some(id) }) if *id == ids[1]
            )
        },
    )
    .await;

    let first = ids[0];
    completed(harness.with_queue(move |queue| queue.select(first, Transition::None)))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "switching back to the track `play` consumed was rejected: {error} — the \
                 queue still reports it as holding a resource the player no longer has"
            )
        });
    completed(harness.with_queue(|queue| queue.clear()))
        .await
        .expect("the queue clears before teardown");
    completed(harness.close()).await;
    hang_reset!();
}
