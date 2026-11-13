#![cfg(not(target_arch = "wasm32"))]

//! A deck plays the item it was given and nothing else on its own: past the
//! item's end it still leads that item until its owner selects another.

use kithara::{
    events::TrackId,
    queue::{ActionAtItemEnd, TrackSource, Transition},
};
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions};
use kithara_test_fixtures::assets;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
/// Several times the item, so the deck renders well past its end.
const BLOCKS: usize = 40;
const LEVEL: f32 = 0.25;

#[kithara::test(tokio)]
async fn a_bare_deck_keeps_its_item_past_the_end() {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let item = TrackId::allocate();
    let source = TrackSource::Uri(
        assets::fixed_constant_wav_cd_quarter_4410()
            .path()
            .expect("prepared quarter WAV")
            .to_string_lossy()
            .into_owned(),
    );
    let mut events = harness.player().subscribe();
    harness
        .with_queue(move |player| {
            player.set_action_at_item_end(ActionAtItemEnd::None);
            player
                .append_with_id(item, source)
                .expect("append the item");
            player
                .select(item, Transition::None)
                .expect("select the item");
            player.play();
        })
        .await;

    kithara_integration_tests::waits::wait_for_loader_done_event(
        &mut events,
        harness.player(),
        item,
        kithara_integration_tests::offline::LOCAL_LOAD_DEADLINE,
    )
    .await
    .expect("the selected item is loaded");

    let mut peak = 0.0_f32;
    for _ in 0..BLOCKS {
        let block = harness.render(BLOCK_FRAMES).await;
        peak = block.iter().map(|sample| sample.abs()).fold(peak, f32::max);
        let _ = harness.tick_and_drain().await;
    }
    let current = harness.player().current().map(|entry| entry.id);
    harness.close().await;

    assert!(
        peak > LEVEL * 0.5,
        "the selected item must be heard; peak={peak}"
    );
    assert_eq!(
        current,
        Some(item),
        "a deck without an owner must keep its item"
    );
}
