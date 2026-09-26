#![cfg(not(target_arch = "wasm32"))]

//! Completion events are published for whichever track in the player's
//! arena reached EOF or failed, not for the one being heard:
//! `PlayerImpl::process_notifications` walks every active slot, and a slot
//! holds more than one track. An orphaned slot decoding ahead, or the
//! outgoing half of a crossfade, reaches its own end while the current
//! track has minutes left. The player names the role in `item`; only
//! `ItemRole::Leading` may advance the queue.

use kithara::{
    audio::DecodeErrorKind,
    events::{EventReceiver, SlotId, TrackId},
    platform::sync::Arc,
    play::{ItemRole, PlaybackFault, PlayerEvent, TrackRef},
    queue::{AdvanceReason, QueueControl, QueueEvent, TrackStatus, Transition},
};
use kithara_integration_tests::{
    event::TestEvent,
    kithara,
    offline::{
        OfflinePlayer, OfflinePlayerOptions, append_loaded, asset_source,
        offline_queue_fixture_with_options,
    },
};
use kithara_test_fixtures::{assets, signal::mean_abs};

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
/// ≈ 0.74 s of rendered audio — far short of the 30 s tracks.
const WARMUP_BLOCKS: usize = 64;
const CROSSFADE_SECS: f32 = 1.0;
const OUTGOING_EOF_BLOCKS: usize = 192;

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

/// Three tracks loaded through the queue, with the first standing in for a
/// non-leading slot. The files stay alive as long as the queue may read them.
struct NonLeadingFixture {
    harness: OfflinePlayer,
    queue: QueueControl<TestPools>,
    stale: TrackRef,
    current: TrackId,
}

async fn non_leading_fixture() -> NonLeadingFixture {
    let (harness, queue) = offline_queue_fixture_with_options(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let files = [
        assets::constant_wav_quiet_30s(),
        assets::constant_wav_loud_30s(),
        assets::constant_wav_quiet_30s(),
    ];
    let stale = append_loaded(&harness, &queue, &files[0]).await;
    let current = append_loaded(&harness, &queue, &files[1]).await;
    let _next = append_loaded(&harness, &queue, &files[2]).await;
    NonLeadingFixture {
        harness,
        queue,
        stale: TrackRef::new(stale, SlotId::new(0), Arc::from(asset_source(&files[0]))),
        current,
    }
}

async fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayer,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(block_budget * BLOCK_FRAMES * usize::from(CHANNELS));
    for _ in 0..block_budget {
        let _ = harness.run(queue, |q| q.tick()).await;
        pcm.extend(harness.render(BLOCK_FRAMES).await);
    }
    pcm
}

fn publish_completion(
    harness: &OfflinePlayer,
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
        Completion::Failure => PlayerEvent::ItemDidFail {
            item,
            fault: PlaybackFault::Decode(DecodeErrorKind::InvalidData),
        },
    };
    harness.player().bus().publish(TestEvent::Player(event));
}

/// Field log, 2026-08-26: a background HLS slot hit EOF 5 s after the
/// current track started and the queue advanced on it, cutting a track
/// with minutes left. The queue must key the advance on the track that
/// ended being the current one.
#[kithara::test(tokio, flash(false))]
#[case::background_eof(Completion::Eof, NonLeadingRole::Background)]
#[case::outgoing_eof(Completion::Eof, NonLeadingRole::Outgoing)]
#[case::background_failure(Completion::Failure, NonLeadingRole::Background)]
async fn non_leading_completion_does_not_advance_the_queue(
    #[case] completion: Completion,
    #[case] role: NonLeadingRole,
) {
    let fixture = non_leading_fixture().await;
    let (harness, queue) = (&fixture.harness, &fixture.queue);
    let stale_id = fixture.stale.id;
    let current = fixture.current;

    harness
        .run(queue, move |q| q.select(current, Transition::None))
        .await
        .expect("select the current track");
    let _ = render_loop(queue, harness, WARMUP_BLOCKS).await;

    publish_completion(harness, completion, role, fixture.stale.clone());
    let _ = render_loop(queue, harness, WARMUP_BLOCKS).await;

    assert_eq!(
        queue.current_index(),
        Some(1),
        "{completion:?} from a track that is not current must leave the current track selected"
    );

    if completion == Completion::Failure {
        let status = queue
            .track(stale_id)
            .map(|entry| entry.status)
            .expect("the background entry must still be in the queue");
        assert!(
            !matches!(status, TrackStatus::Failed(_)),
            "a background track's failure must not mark the entry failed: {status:?}"
        );
    }
    let NonLeadingFixture { harness, queue, .. } = fixture;
    drop(queue);
    harness.close().await;
}

/// The audible half of the same defect: the listener hears the current
/// track handed over to the successor while it is still playing.
#[kithara::test(tokio, flash(false))]
#[case::eof(Completion::Eof)]
#[case::failure(Completion::Failure)]
async fn background_completion_does_not_cut_the_current_track_audio(
    #[case] completion: Completion,
) {
    let fixture = non_leading_fixture().await;
    let (harness, queue) = (&fixture.harness, &fixture.queue);
    let current = fixture.current;

    harness
        .run(queue, move |q| q.select(current, Transition::None))
        .await
        .expect("select the current track");
    let before_pcm = render_loop(queue, harness, WARMUP_BLOCKS).await;
    let before = mean_abs(&before_pcm[before_pcm.len() / 2..]);
    assert!(
        before > 0.005,
        "the current track must be audible before the background {completion:?}: mean={before}"
    );

    publish_completion(
        harness,
        completion,
        NonLeadingRole::Background,
        fixture.stale.clone(),
    );
    let after_pcm = render_loop(queue, harness, WARMUP_BLOCKS).await;
    let after = mean_abs(&after_pcm[after_pcm.len() / 2..]);

    assert!(
        after > before / 2.0,
        "the current track must keep sounding through a background track's {completion:?} — \
         the quieter successor took over instead: before={before}, after={after}"
    );
    let NonLeadingFixture { harness, queue, .. } = fixture;
    drop(queue);
    harness.close().await;
}

/// A real outgoing EOF must leave the promoted successor selected and audible.
///
/// `ItemRole::Outgoing` is produced by the player from the crossfade's actual
/// terminal notification.
#[kithara::test(tokio, flash(false))]
async fn outgoing_eof_does_not_advance_the_promoted_successor() {
    let (harness, queue) = offline_queue_fixture_with_options(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let outgoing = assets::constant_wav_quiet_30s();
    let successor = assets::constant_wav_loud_30s();
    let first = append_loaded(&harness, &queue, &outgoing).await;
    let second = append_loaded(&harness, &queue, &successor).await;
    let mut events: EventReceiver<QueueEvent> = queue.subscribe();

    harness
        .run(&queue, move |q| q.select(first, Transition::None))
        .await
        .expect("select the outgoing track");
    let _ = render_loop(&queue, &harness, 16).await;
    while events.try_recv().is_ok() {}

    harness
        .run(&queue, move |q| q.next(Transition::Crossfade))
        .await
        .expect("start a real crossfade to the successor");
    let pcm = render_loop(&queue, &harness, OUTGOING_EOF_BLOCKS).await;

    assert_eq!(queue.current_index(), Some(1));
    assert_eq!(queue.current().map(|entry| entry.id), Some(second));
    let tail = mean_abs(&pcm[pcm.len() / 2..]);
    assert!(
        tail > 0.005,
        "the promoted successor must remain audible after outgoing EOF: mean={tail}"
    );

    let mut advances = Vec::new();
    while let Ok(envelope) = events.try_recv() {
        if let QueueEvent::CurrentTrackAdvance { id, reason } = envelope.event {
            advances.push((id, reason));
        }
    }
    assert_eq!(
        advances,
        vec![(Some(second), AdvanceReason::UserNext)],
        "outgoing EOF must not create a second queue advance"
    );

    drop(queue);
    harness.close().await;
}
