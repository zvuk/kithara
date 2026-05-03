#![cfg(not(target_arch = "wasm32"))]

use kithara_decode::PcmSpec;
use kithara_play::{PlayerConfig, Resource, internal::offline::resource_from_reader};
use kithara_test_utils::kithara;

use super::offline_player_harness::OfflinePlayerHarness;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
/// 100 ms of stereo audio at 44.1 kHz: `4_410` frames × 2 channels.
const TARGET_SAMPLES: usize = 8_820;
const MAX_RENDERED_FRAMES: usize = 9_000;

fn mock_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE,
    }
}

fn make_resource(duration_secs: f64) -> Resource {
    resource_from_reader(kithara_audio::mock::TestPcmReader::new(
        mock_spec(),
        duration_secs,
    ))
}

#[kithara::test]
fn offline_harness_smoke() {
    let harness = OfflinePlayerHarness::with_sample_rate(PlayerConfig::default(), SAMPLE_RATE);
    harness.player().insert(make_resource(0.1), None, None);
    harness.player().insert(make_resource(0.1), None, None);

    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");

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
