#![cfg(not(target_arch = "wasm32"))]

//! Completion events are published for whichever track in the player's
//! arena reached EOF or failed, not for the one being heard:
//! `PlayerImpl::process_notifications` walks every active slot, and a slot
//! holds more than one track. An orphaned slot decoding ahead, or the
//! outgoing half of a crossfade, reaches its own end while the current
//! track has minutes left. The player names the role in `item`; only
//! `ItemRole::Leading` may advance the queue.
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
    offline::{
        OfflinePlayerHarness, mean_abs, offline_queue_fixture, resource_from_reader_with_src,
    },
};

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
/// ≈ 0.74 s of rendered audio — far short of `TRACK_SECS`.
const WARMUP_BLOCKS: usize = 64;
const TRACK_SECS: f64 = 30.0;
const LOUD: f32 = 0.80;
const QUIET: f32 = 0.10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Completion {
    Eof,
    Failure,
}

#[derive(Clone, Copy, Debug)]
enum NonLeadingRole {
    Background,
    Outgoing,
}

/// Load a track whose player-side `src` is its queue URI, the way a real
/// source arrives.
fn loaded_track(queue: &QueueControl<TestPools>, value: f32) -> (TrackId, Arc<str>) {
    let id = queue.register_for_test();
    let src: Arc<str> = Arc::from(format!("test://memory/{}", id.as_u64()));
    let spec = AudioSpec {
        channels: CHANNELS,
        sample_rate: NonZero::new(SAMPLE_RATE).unwrap(),
    };
    queue.complete_load_for_test(
        id,
        resource_from_reader_with_src(
            TestPcmReader::with_value(spec, TRACK_SECS, value),
            Arc::clone(&src),
        ),
    );
    (id, src)
}

fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayerHarness,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::new();
    for _ in 0..block_budget {
        let _ = queue.tick();
        pcm.extend(harness.render(BLOCK_FRAMES));
    }
    pcm
}

/// Three loaded tracks with the first standing in for a non-leading slot.
fn non_leading_fixture() -> (
    OfflinePlayerHarness,
    QueueControl<TestPools>,
    TrackRef,
    TrackId,
) {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE);
    let (stale, stale_src) = loaded_track(&queue, QUIET);
    let (current, _) = loaded_track(&queue, LOUD);
    let (_next, _) = loaded_track(&queue, QUIET);
    (
        harness,
        queue,
        TrackRef::new(stale, SlotId::new(0), stale_src),
        current,
    )
}

fn publish_completion(
    harness: &OfflinePlayerHarness,
    completion: Completion,
    role: NonLeadingRole,
    track: TrackRef,
) {
    let item = match role {
        NonLeadingRole::Background => ItemRole::Background(track),
        NonLeadingRole::Outgoing => ItemRole::Outgoing(track),
    };
    let event = match completion {
        Completion::Eof => PlayerEvent::ItemDidPlayToEnd { item },
        Completion::Failure => PlayerEvent::ItemDidFail { item },
    };
    harness.player().bus().publish(Event::Player(event));
}

/// Field log, 2026-08-26: a background HLS slot hit EOF 5 s after the
/// current track started and the queue advanced on it, cutting a track
/// with minutes left. The queue must key the advance on the track that
/// ended being the current one.
#[kithara::test(tokio)]
#[case::background_eof(Completion::Eof, NonLeadingRole::Background)]
#[case::outgoing_eof(Completion::Eof, NonLeadingRole::Outgoing)]
#[case::background_failure(Completion::Failure, NonLeadingRole::Background)]
async fn non_leading_completion_does_not_advance_the_queue(
    #[case] completion: Completion,
    #[case] role: NonLeadingRole,
) {
    let (harness, queue, stale, current) = non_leading_fixture();
    let stale_id = stale.id;

    queue
        .select(current, Transition::None)
        .expect("select the current track");
    let _ = render_loop(&queue, &harness, WARMUP_BLOCKS);

    publish_completion(&harness, completion, role, stale);
    let _ = render_loop(&queue, &harness, WARMUP_BLOCKS);

    assert_eq!(
        queue.current_index(),
        Some(1),
        "{completion:?} from a track that is not current must leave the current track selected"
    );

    if completion == Completion::Failure {
        let status = queue
            .tracks()
            .into_iter()
            .find(|entry| entry.id == stale_id)
            .map(|entry| entry.status)
            .expect("the background entry must still be in the queue");
        assert!(
            !matches!(status, TrackStatus::Failed(_)),
            "a background track's failure must not mark the entry failed: {status:?}"
        );
    }
}

/// The audible half of the same defect: the listener hears the current
/// track handed over to the successor while it is still playing.
#[kithara::test(tokio)]
#[case::eof(Completion::Eof)]
#[case::failure(Completion::Failure)]
async fn background_completion_does_not_cut_the_current_track_audio(
    #[case] completion: Completion,
) {
    let (harness, queue, stale, current) = non_leading_fixture();

    queue
        .select(current, Transition::None)
        .expect("select the current track");
    let before_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS);
    let before = mean_abs(&before_pcm[before_pcm.len() / 2..]);
    assert!(
        before > 0.005,
        "the current track must be audible before the background {completion:?}: mean={before}"
    );

    publish_completion(&harness, completion, NonLeadingRole::Background, stale);
    let after_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS);
    let after = mean_abs(&after_pcm[after_pcm.len() / 2..]);

    assert!(
        after > before / 2.0,
        "the current track must keep sounding through a background track's {completion:?} — \
         the quieter successor took over instead: before={before}, after={after}"
    );
}
