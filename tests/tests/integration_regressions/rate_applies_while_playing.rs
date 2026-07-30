#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{decode::PcmSpec, play::Resource};
use kithara_integration_tests::offline::resource_from_reader;

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const WARMUP_BLOCKS: usize = 8;
/// Wide enough for the rate-1.0 baseline to reach silence inside the window.
/// A window that clips the baseline makes both sides saturate at the cap and
/// the comparison below vacuous.
const MEASURE_BLOCKS: usize = 200;
const FAST_RATE: f32 = 2.0;

fn make_resource(duration_secs: f64) -> Resource {
    resource_from_reader(kithara_integration_tests::audio_mock::TestPcmReader::new(
        PcmSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
        duration_secs,
    ))
}

/// Changing the rate during playback must immediately change
/// source consumption without a pause/play cycle.
///
/// This compares equal-sized offline pulls: at twice the rate, the same source
/// must reach silence in fewer rendered blocks.
#[kithara::test]
fn rate_change_while_playing_changes_consumption() {
    let baseline = blocks_until_silence(None);
    let accelerated = blocks_until_silence(Some(FAST_RATE));

    assert!(
        baseline < MEASURE_BLOCKS,
        "the rate-1.0 baseline must drain inside the measured window, \
         got {baseline} of {MEASURE_BLOCKS} blocks — the comparison below would \
         compare two saturated caps"
    );
    assert!(
        accelerated < baseline,
        "at rate {FAST_RATE} the source must drain in fewer blocks \
         than at rate 1.0, got {accelerated} vs baseline {baseline}"
    );
}

fn blocks_until_silence(rate: Option<f32>) -> usize {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SAMPLE_RATE,
    );
    harness.player().insert(make_resource(1.0), None, None);
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");

    for _ in 0..WARMUP_BLOCKS {
        let _ = harness.render(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
    }

    if let Some(rate) = rate {
        harness.player().set_default_rate(rate);
    }

    let mut blocks = 0usize;
    for _ in 0..MEASURE_BLOCKS {
        let block = harness.render(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
        blocks = blocks.saturating_add(1);
        if block.iter().all(|sample| sample.abs() == 0.0) {
            break;
        }
    }
    blocks
}
