#![cfg(not(target_arch = "wasm32"))]

//! Two queue entries may name the same URL: a playlist that repeats a
//! track, or one asset reachable under a single address. A player event
//! must resolve to the entry that actually played. Resolving it by
//! source alone answers with whichever entry holds that URL first,
//! which is the wrong track as soon as the second copy is the one
//! playing.
use std::num::NonZero;

use kithara::{
    self,
    events::{Event, ItemRole, PlayerEvent, SlotId, TrackId, TrackRef, TrackStatus},
    platform::sync::Arc,
    queue::{QueueControl, Transition, test_utils::QueueProbe},
    signal::AudioSpec,
};
use kithara_integration_tests::{
    audio_mock::TestPcmReader,
    offline::{OfflinePlayerHarness, offline_queue_fixture, resource_from_reader_with_src},
};
use kithara_test_fixtures::integration_fixtures::constant_loud;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
/// ≈ 0.74 s of rendered audio — far short of `TRACK_SECS`.
const WARMUP_BLOCKS: usize = 64;
const TRACK_SECS: f64 = 30.0;
const REPEATED_SRC: &str = "https://example.com/repeat.mp3";

async fn load(
    harness: &OfflinePlayerHarness,
    queue: &QueueControl<TestPools>,
    id: TrackId,
    constant_loud: &'static [u8],
) {
    let spec = AudioSpec::new(
        CHANNELS,
        NonZero::new(SAMPLE_RATE).expect("sample rate is non-zero"),
    );
    harness
        .run(queue, move |q| {
            q.complete_load_for_test(
                id,
                resource_from_reader_with_src(
                    TestPcmReader::from_pcm(spec, TRACK_SECS, constant_loud),
                    Arc::from(REPEATED_SRC),
                ),
            )
        })
        .await;
}

async fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayerHarness,
    block_budget: usize,
) {
    for _ in 0..block_budget {
        let _ = harness.run(queue, |q| q.tick()).await;
        let _ = harness.render(BLOCK_FRAMES).await;
    }
}

fn status_of(queue: &QueueControl<TestPools>, id: TrackId) -> TrackStatus {
    queue
        .tracks()
        .into_iter()
        .find(|entry| entry.id == id)
        .map(|entry| entry.status)
        .expect("the entry must still be in the queue")
}

/// The failing track is the *second* entry carrying this URL.
async fn fixture_playing_the_second_copy(
    constant_loud: &'static [u8],
) -> (
    OfflinePlayerHarness,
    QueueControl<TestPools>,
    TrackId,
    TrackId,
) {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let first = harness
        .run(&queue, move |q| q.append(REPEATED_SRC))
        .await
        .expect("append first copy");
    let playing = harness
        .run(&queue, move |q| q.append(REPEATED_SRC))
        .await
        .expect("append second copy");
    load(&harness, &queue, first, constant_loud).await;
    load(&harness, &queue, playing, constant_loud).await;

    harness
        .run(&queue, move |q| q.select(playing, Transition::None))
        .await
        .expect("select the second copy");
    render_loop(&queue, &harness, WARMUP_BLOCKS).await;

    (harness, queue, first, playing)
}

fn publish_leading_failure(harness: &OfflinePlayerHarness, id: TrackId) {
    harness
        .player()
        .bus()
        .publish(Event::Player(PlayerEvent::ItemDidFail {
            item: ItemRole::Leading(TrackRef::new(id, SlotId::new(0), Arc::from(REPEATED_SRC))),
        }));
}

#[kithara::test(tokio)]
#[case::played_entry(true)]
#[case::same_url_entry(false)]
async fn a_failure_only_flags_the_entry_that_played(
    #[case] played_entry: bool,
    constant_loud: &'static [u8],
) {
    let (harness, queue, first, playing) = fixture_playing_the_second_copy(constant_loud).await;

    publish_leading_failure(&harness, playing);
    render_loop(&queue, &harness, WARMUP_BLOCKS).await;

    let id = if played_entry { playing } else { first };
    let status = status_of(&queue, id);
    assert_eq!(
        matches!(status, TrackStatus::Failed(_)),
        played_entry,
        "only the entry that played may be flagged: {status:?}"
    );
    drop(queue);
    harness.close().await;
}
