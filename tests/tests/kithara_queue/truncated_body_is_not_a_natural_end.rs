#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    events::{AdvanceReason, Event, QueueEvent, TrackId},
    platform::{
        sync::Arc,
        time::{self, Duration},
    },
    play::{Resource, ResourceConfig, ResourceSrc, player::PlayerControl},
    queue::{Queue, QueueConfig, QueueControl, Transition, test_utils::QueueProbe},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, kithara,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions},
    temp_dir,
};
use kithara_test_fixtures::assets;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const CROSSFADE_SECS: f32 = 1.0;
/// Two six-second tracks plus slack; the loop leaves as soon as the queue ends.
const BLOCK_BUDGET: usize = 3_000;
/// How much of the first track's body arrives before it stops, as a fraction
/// of the whole: far enough in that the header and seconds of audio are there,
/// far enough from the end that no crossfade could reach it.
const DELIVERED_NUMERATOR: usize = 2;
const DELIVERED_DENOMINATOR: usize = 5;

async fn open_resource(
    player: &PlayerControl<TestPools>,
    src: ResourceSrc,
    cache_dir: &Path,
) -> Resource {
    let config = ResourceConfig::<TestPools>::for_src(src)
        .store(kithara_integration_tests::disk_asset_store(cache_dir))
        .build();
    let config = player.prepare_config(config).expect("prepare resource");
    let mut resource = Resource::new(config).await.expect("open resource");
    let _ = resource.preload().await;
    resource
}

/// What the queue did over the whole playthrough.
#[derive(Default)]
struct QueueLog {
    advances: Vec<(Option<TrackId>, AdvanceReason)>,
    crossfades: usize,
    ended: bool,
}

impl QueueLog {
    /// Why the queue moved onto `id` — for the successor, the ways it left the
    /// track whose body stopped.
    fn advances_onto(&self, id: TrackId) -> Vec<AdvanceReason> {
        self.advances
            .iter()
            .filter_map(|&(target, reason)| (target == Some(id)).then_some(reason))
            .collect()
    }
}

/// A whole body served over HTTP, delivered in full or cut short.
fn track_src(server: &TestServerHelper, delivery: Delivery) -> ResourceSrc {
    let handle = server.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(assets::signal_flac_saw_6s().bytes().to_vec()),
            content_type: Some("audio/flac"),
        },
        delivery,
    });
    ResourceSrc::parse(handle.child_url("track.flac").as_str()).expect("valid track URL")
}

/// A body that stops early must not be heard as the track's own end.
///
/// A `200` with no `Content-Length` names no total, so a body that stops after
/// two fifths is framed exactly as a complete one: the net layer reads a clean
/// end, the file layer commits what it wrote as the whole file, and the reader
/// announces an end two fifths in. Taken at face value that announcement arms
/// the crossfade a fade before it and advances the queue after it, which is the
/// fade a listener hears in the middle of a track. The one number a lost body
/// cannot move is the length the media itself declares.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(30)
)]
async fn a_truncated_body_does_not_advance_the_queue_as_a_natural_end(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let whole = assets::signal_flac_saw_6s().bytes().len();
    let truncated = track_src(
        &server,
        Delivery::UnsizedEarlyClose {
            after_bytes: whole * DELIVERED_NUMERATOR / DELIVERED_DENOMINATOR,
        },
    );
    let successor = track_src(&server, Delivery::Range);

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    );
    let mut config = QueueConfig::builder().player(harness.take_player()).build();
    config.should_autoplay = false;
    let queue: QueueControl<TestPools> = harness.insert_control(Queue::new(config));

    let mut tracks = Vec::with_capacity(2);
    for (index, src) in [truncated, successor].into_iter().enumerate() {
        let resource = open_resource(
            harness.player(),
            src,
            &temp_dir.path().join(format!("track{index}")),
        )
        .await;
        tracks.push(queue.insert_loaded_for_test(resource));
    }
    let (first, second) = (tracks[0], tracks[1]);
    queue
        .select(first, Transition::Crossfade)
        .expect("select the first track");

    let mut receiver = queue.subscribe();
    let mut log = QueueLog::default();
    for _ in 0..BLOCK_BUDGET {
        let _ = queue.tick();
        let _ = harness.render(BLOCK_FRAMES);
        while let Ok(envelope) = receiver.try_recv() {
            match envelope.event {
                Event::Queue(QueueEvent::CurrentTrackAdvance { id, reason }) => {
                    log.advances.push((id, reason));
                }
                Event::Queue(QueueEvent::CrossfadeStarted { .. }) => log.crossfades += 1,
                Event::Queue(QueueEvent::QueueEnded) => log.ended = true,
                _ => {}
            }
        }
        if log.ended {
            break;
        }
        time::sleep(Duration::from_millis(1)).await;
    }

    let left_by = log.advances_onto(second);
    assert!(
        !left_by.is_empty(),
        "the queue must leave a track whose body stopped, not sit on it: \
         advances={:?} ended={}",
        log.advances,
        log.ended
    );
    assert!(
        !left_by.contains(&AdvanceReason::NaturalEof),
        "a body that stopped at {DELIVERED_NUMERATOR}/{DELIVERED_DENOMINATOR} of \
         the track must not advance the queue as a track that played to its \
         end: left_by={left_by:?}"
    );
    assert_eq!(
        log.crossfades, 0,
        "a body that stopped must not cross-fade into the next track: \
         left_by={left_by:?}"
    );
}
