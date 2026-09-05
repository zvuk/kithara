#![cfg(not(target_arch = "wasm32"))]

use kithara::{self, events::TrackId, play::Resource};
use kithara_integration_tests::{
    offline::{OfflinePlayerHarness, OfflinePlayerOptions, resource_from_reader},
    test_defaults::Consts,
};

const BLOCK_FRAMES: usize = 512;
/// 100 ms of stereo audio at 44.1 kHz: `4_410` frames × 2 channels.
const TARGET_SAMPLES: usize = 8_820;
const MAX_RENDERED_FRAMES: usize = 9_000;

fn make_resource(duration_secs: f64) -> Resource {
    resource_from_reader(kithara_integration_tests::audio_mock::TestPcmReader::new(
        Consts::AUDIO_SPEC,
        duration_secs,
    ))
}

#[kithara::test]
fn offline_harness_smoke() {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        Consts::SAMPLE_RATE,
    );
    harness.with_player(|player| {
        player.insert(make_resource(0.1), TrackId::allocate(), None);
        player.insert(make_resource(0.1), TrackId::allocate(), None);
        player
            .select_item(0, true)
            .expect("select first queue item");
    });

    let mut rendered: Vec<f32> = Vec::new();
    let mut total_frames: usize = 0;
    while rendered.len() < TARGET_SAMPLES && total_frames < MAX_RENDERED_FRAMES {
        let block = harness.render(BLOCK_FRAMES);
        rendered.extend_from_slice(&block);
        total_frames = total_frames.saturating_add(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
    }

    assert!(!rendered.is_empty());
    assert!(rendered.iter().any(|sample| sample.abs() > 0.0));
}
