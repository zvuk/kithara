#![cfg(not(target_arch = "wasm32"))]

//! Two queue entries may name the same URL: a playlist that repeats a
//! track, or one asset reachable under a single address. A player event
//! must resolve to the entry that actually played. Resolving it by
//! source alone answers with whichever entry holds that URL first,
//! which is the wrong track as soon as the second copy is the one
//! playing.

use kithara::{
    audio::DecodeErrorKind,
    events::{SlotId, TrackId},
    platform::sync::Arc,
    play::{ItemRole, PlaybackFault, PlayerEvent, TrackRef},
    queue::{QueueControl, TrackStatus, Transition},
};
use kithara_integration_tests::{
    event::TestEvent,
    kithara,
    offline::{OfflinePlayerHarness, offline_queue_fixture},
};
use kithara_test_fixtures::{asset::Asset, assets};

use crate::{
    bufpool_ext::TestPools,
    loader_fixture::{append_source_loaded, source},
};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
/// ≈ 0.74 s of rendered audio — far short of the 30 s track.
const WARMUP_BLOCKS: usize = 64;
const EOF_BLOCK_BUDGET: usize = 128;

async fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayerHarness,
    block_budget: usize,
) {
    for _ in 0..block_budget {
        let _ = harness.run(queue, kithara::queue::QueueControl::tick).await;
        let _ = harness.render(BLOCK_FRAMES).await;
    }
}

fn status_of(queue: &QueueControl<TestPools>, id: TrackId) -> TrackStatus {
    queue
        .track(id)
        .map(|entry| entry.status)
        .expect("the entry must still be in the queue")
}

/// Two entries appended from one file, with the second one selected.
struct SecondCopyPlaying {
    harness: OfflinePlayerHarness,
    queue: QueueControl<TestPools>,
    source: String,
    first: TrackId,
    playing: TrackId,
}

async fn fixture_playing_the_second_copy(track: &Asset) -> SecondCopyPlaying {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source = source(track);
    let first = append_source_loaded(&harness, &queue, source.clone()).await;
    let playing = append_source_loaded(&harness, &queue, source.clone()).await;

    harness
        .run(&queue, move |q| q.select(playing, Transition::None))
        .await
        .expect("select the second copy");

    SecondCopyPlaying {
        harness,
        queue,
        source,
        first,
        playing,
    }
}

#[kithara::test(tokio, flash(false))]
#[case::played_entry(true)]
#[case::same_url_entry(false)]
async fn a_failure_only_flags_the_entry_that_played(#[case] played_entry: bool) {
    let SecondCopyPlaying {
        harness,
        queue,
        source,
        first,
        playing,
    } = fixture_playing_the_second_copy(&assets::constant_wav_loud_30s()).await;
    render_loop(&queue, &harness, WARMUP_BLOCKS).await;

    harness
        .player()
        .bus()
        .publish(TestEvent::Player(PlayerEvent::ItemDidFail {
            item: ItemRole::Leading(TrackRef::new(playing, SlotId::new(0), Arc::from(source))),
            fault: PlaybackFault::Decode(DecodeErrorKind::InvalidData),
        }));
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

#[kithara::test(tokio, flash(false))]
async fn second_entry_with_the_same_source_owns_its_real_eof() {
    let SecondCopyPlaying {
        harness,
        queue,
        source: _source,
        first,
        playing,
    } = fixture_playing_the_second_copy(&assets::constant_wav_loud_0_5s()).await;
    render_loop(&queue, &harness, EOF_BLOCK_BUDGET).await;

    assert_eq!(
        status_of(&queue, playing),
        TrackStatus::Consumed,
        "the selected duplicate must own its natural EOF"
    );
    assert_eq!(
        status_of(&queue, first),
        TrackStatus::Consumed,
        "the initially selected duplicate remains distinct from the entry that reached EOF"
    );
    assert!(
        queue.current().is_none(),
        "queue must be inactive after its terminal EOF"
    );

    drop(queue);
    harness.close().await;
}
