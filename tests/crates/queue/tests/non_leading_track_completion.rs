#![cfg(not(target_arch = "wasm32"))]

//! The outgoing half of a real crossfade reaches its terminal notification
//! after its successor is already current. That completion must not advance
//! the queue for a second time or cut the successor.

use kithara::{
    events::EventReceiver,
    queue::{AdvanceReason, QueueControl, QueueEvent, Transition},
};
use kithara_integration_tests::{
    event::TestEvent,
    kithara,
    offline::{
        OfflinePlayerHarness, OfflinePlayerOptions, mean_abs, offline_queue_fixture_with_options,
    },
};
use kithara_test_fixtures::integration_fixtures::{constant_loud, constant_quiet};

use crate::{
    bufpool_ext::TestPools,
    loader_fixture::{LocalWav, append_loaded},
};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const TRACK_SECS: f64 = 30.0;
const CROSSFADE_SECS: f32 = 1.0;
const BLOCK_FRAMES: usize = 512;
const OUTGOING_EOF_BLOCKS: usize = 192;

async fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayerHarness,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(block_budget * BLOCK_FRAMES * usize::from(CHANNELS));
    for _ in 0..block_budget {
        let _ = harness.run(queue, |q| q.tick()).await;
        pcm.extend(harness.render(BLOCK_FRAMES).await);
    }
    pcm
}

/// A real outgoing EOF must leave the promoted successor selected and audible.
///
/// `ItemRole::Outgoing` is produced by the player from the crossfade's actual
/// terminal notification; this test never publishes or injects a player event.
#[kithara::test(tokio, flash(false))]
async fn outgoing_eof_does_not_advance_the_promoted_successor(
    constant_quiet: &'static [u8],
    constant_loud: &'static [u8],
) {
    let (harness, queue) = offline_queue_fixture_with_options(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let outgoing = LocalWav::constant(
        "outgoing-eof",
        SAMPLE_RATE,
        CHANNELS,
        TRACK_SECS,
        constant_quiet,
    );
    let successor = LocalWav::constant(
        "outgoing-successor",
        SAMPLE_RATE,
        CHANNELS,
        TRACK_SECS,
        constant_loud,
    );
    let first = append_loaded(&harness, &queue, &outgoing).await;
    let second = append_loaded(&harness, &queue, &successor).await;
    let mut events: EventReceiver<TestEvent> = queue.subscribe();

    harness
        .run(&queue, move |q| q.select(first, Transition::None))
        .await
        .expect("select the outgoing track");
    let _ = render_loop(&queue, &harness, 16).await;
    // The initial select is setup; only the subsequent user advance belongs
    // to the outgoing-EOF assertion.
    while events.try_recv().is_ok() {}

    harness
        .run(&queue, move |q| {
            q.advance_to_next(Transition::Crossfade, AdvanceReason::UserNext)
        })
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
        if let TestEvent::Queue(QueueEvent::CurrentTrackAdvance { id, reason }) = envelope.event {
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
