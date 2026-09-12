#![cfg(not(target_arch = "wasm32"))]

//! Repeated source URLs remain distinct queue entries through a real EOF.

use kithara::queue::{QueueControl, TrackStatus, Transition};
use kithara_integration_tests::{
    kithara,
    offline::{OfflinePlayerHarness, offline_queue_fixture},
};
use kithara_test_fixtures::integration_fixtures::constant_loud;

use crate::{
    bufpool_ext::TestPools,
    loader_fixture::{LocalWav, append_source_loaded},
};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const TRACK_SECS: f64 = 0.5;
const BLOCK_FRAMES: usize = 512;
const EOF_BLOCK_BUDGET: usize = 128;

async fn render_to_eof(queue: &QueueControl<TestPools>, harness: &OfflinePlayerHarness) {
    for _ in 0..EOF_BLOCK_BUDGET {
        let _ = harness.run(queue, |queue| queue.tick()).await;
        let _ = harness.render(BLOCK_FRAMES).await;
    }
}

#[kithara::test(tokio, flash(false))]
async fn second_entry_with_the_same_source_owns_its_real_eof(constant_loud: &'static [u8]) {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source = LocalWav::constant(
        "duplicate-source",
        SAMPLE_RATE,
        CHANNELS,
        TRACK_SECS,
        constant_loud,
    );
    let first = append_source_loaded(&harness, &queue, source.source()).await;
    let second = append_source_loaded(&harness, &queue, source.source()).await;

    harness
        .run(&queue, move |queue| queue.select(second, Transition::None))
        .await
        .expect("select the second entry");
    render_to_eof(&queue, &harness).await;

    assert_eq!(
        queue.track(second).map(|entry| entry.status),
        Some(TrackStatus::Consumed),
        "the selected duplicate must own its natural EOF"
    );
    assert_eq!(
        queue.track(first).map(|entry| entry.status),
        Some(TrackStatus::Loaded),
        "the non-playing duplicate must remain loaded"
    );
    assert!(
        queue.current().is_none(),
        "queue must be inactive after its terminal EOF"
    );

    drop(queue);
    harness.close().await;
}
