#![cfg(not(target_arch = "wasm32"))]

//! A whole MPEG body fetched over HTTP must be heard as ending when it ends.
//!
//! The census reads a track through HLS segments, through a local FLAC file
//! and through a whole FLAC body on a server, but never through MPEG - and
//! MPEG is the container whose length is least reconciled with its audio: the
//! figure comes from a Xing frame count, and encoder delay and padding sit
//! either side of it. A queue that mistakes that seam for a track boundary
//! cuts the track short, which is what a listener hears as a fade in the
//! middle of one.

use std::path::Path;

use kithara::{
    events::{AdvanceReason, Event, QueueEvent, TrackId},
    platform::{
        sync::Arc,
        time::{self, Duration},
    },
    play::{Resource, ResourceConfig, ResourceSrc},
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
const NO_CROSSFADE_SECS: f32 = 0.0;
/// Two tracks plus slack; the loop leaves as soon as the queue ends.
const BLOCK_BUDGET: usize = 3_000;

async fn open_resource(
    harness: &OfflinePlayerHarness,
    src: ResourceSrc,
    cache_dir: &Path,
) -> Resource {
    let config = ResourceConfig::<TestPools>::for_src(src)
        .store(kithara_integration_tests::disk_asset_store(cache_dir))
        .build();
    let config = harness
        .with_player(move |player| player.prepare_config(config))
        .await
        .expect("prepare resource");
    let mut resource = Resource::new(config).await.expect("open resource");
    let _ = resource.preload().await;
    resource
}

/// What the queue did over the whole playthrough.
#[derive(Default)]
struct QueueLog {
    advances: Vec<(Option<TrackId>, AdvanceReason)>,
    crossfades: usize,
    load_failures: Vec<String>,
    ended: bool,
}

impl QueueLog {
    /// Why the queue moved onto `id` - for the successor, the ways it left the
    /// track before it.
    fn advances_onto(&self, id: TrackId) -> Vec<AdvanceReason> {
        self.advances
            .iter()
            .filter_map(|&(target, reason)| (target == Some(id)).then_some(reason))
            .collect()
    }
}

/// One whole MPEG body, served over HTTP as a range-capable response.
fn track_src(server: &TestServerHelper) -> ResourceSrc {
    let handle = server.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(assets::signal_mp3_saw_2s().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    ResourceSrc::parse(handle.child_url("track.mp3").as_str()).expect("valid track URL")
}

/// Play a two-track queue from the first track to the end of the queue, and
/// report what the queue did and which track it moved onto.
async fn play_queue(
    crossfade: f32,
    temp_dir: &TestTempDir,
    sources: Vec<ResourceSrc>,
) -> (QueueLog, TrackId) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(crossfade)
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let mut config = QueueConfig::builder().player(harness.take_player()).build();
    config.should_autoplay = false;
    let queue: QueueControl<TestPools> = harness.insert_control(Queue::new(config)).await;

    let mut tracks = Vec::with_capacity(2);
    for (index, source) in sources.into_iter().enumerate() {
        let resource = open_resource(
            &harness,
            source,
            &temp_dir.path().join(format!("track{index}")),
        )
        .await;
        tracks.push(
            harness
                .run(&queue, move |q| q.insert_loaded_for_test(resource))
                .await,
        );
    }
    let (first, second) = (tracks[0], tracks[1]);
    let transition = if crossfade > 0.0 {
        Transition::Crossfade
    } else {
        Transition::None
    };
    harness
        .run(&queue, move |q| q.select(first, transition))
        .await
        .expect("select the first track");

    let mut receiver = queue.subscribe();
    let mut log = QueueLog::default();
    for _ in 0..BLOCK_BUDGET {
        let _ = harness.run(&queue, |q| q.tick()).await;
        let _ = harness.render(BLOCK_FRAMES).await;
        while let Ok(envelope) = receiver.try_recv() {
            match envelope.event {
                Event::Queue(QueueEvent::CurrentTrackAdvance { id, reason }) => {
                    log.advances.push((id, reason));
                }
                Event::Queue(QueueEvent::CrossfadeStarted { .. }) => log.crossfades += 1,
                Event::Queue(QueueEvent::TrackLoadFailed { reason, .. }) => {
                    log.load_failures.push(reason);
                }
                Event::Queue(QueueEvent::QueueEnded) => log.ended = true,
                _ => {}
            }
        }
        if log.ended {
            break;
        }
        time::sleep(Duration::from_millis(1)).await;
    }

    drop(queue);
    harness.close().await;
    (log, second)
}

/// The queue must leave a whole MPEG track the way it leaves a finished one.
async fn mp3_track_ends_rather_than_fails(
    crossfade: f32,
    expected_crossfades: usize,
    temp_dir: &TestTempDir,
    sources: Vec<ResourceSrc>,
) {
    let (log, second) = play_queue(crossfade, temp_dir, sources).await;
    let left_by = log.advances_onto(second);

    assert!(
        !left_by.is_empty(),
        "the queue must move onto the successor: advances={:?} ended={}",
        log.advances,
        log.ended
    );
    assert!(
        !left_by.contains(&AdvanceReason::TrackFailed),
        "a track whose body arrived whole must not be reported as a failure: \
         left_by={left_by:?}"
    );
    assert!(
        log.load_failures.is_empty(),
        "a track whose body arrived whole must reach its end without a load \
         failure: {:?}",
        log.load_failures
    );
    assert_eq!(
        log.crossfades, expected_crossfades,
        "the seam must carry exactly the crossfade the queue was configured \
         for: left_by={left_by:?}"
    );
}

/// Without a crossfade the queue hands over on the render-block grid alone,
/// which is the tightest a seam ever gets.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(30)
)]
async fn a_streamed_mp3_ends_its_track_without_a_crossfade(
    temp_dir: TestTempDir,
    #[future(awt)] mp3_sources: (TestServerHelper, Vec<ResourceSrc>),
) {
    let (_server, sources) = mp3_sources;
    mp3_track_ends_rather_than_fails(NO_CROSSFADE_SECS, 0, &temp_dir, sources).await;
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(30)
)]
async fn a_streamed_mp3_ends_its_track_with_a_crossfade(
    temp_dir: TestTempDir,
    #[future(awt)] mp3_sources: (TestServerHelper, Vec<ResourceSrc>),
) {
    let (_server, sources) = mp3_sources;
    mp3_track_ends_rather_than_fails(CROSSFADE_SECS, 1, &temp_dir, sources).await;
}

#[kithara::fixture]
async fn mp3_sources() -> (TestServerHelper, Vec<ResourceSrc>) {
    let server = TestServerHelper::new().await;
    let sources = (0..2).map(|_| track_src(&server)).collect();
    (server, sources)
}
