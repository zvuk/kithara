#![cfg(not(target_arch = "wasm32"))]

use kithara_decode::PcmSpec;
use kithara_play::{
    PlayerConfig, Resource, impls::offline_backend::OfflineConfig,
    internal::offline::resource_from_reader,
};
use kithara_test_utils::kithara;

use super::offline_player_harness::OfflinePlayerHarness;

fn mock_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 44_100,
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
    let harness = OfflinePlayerHarness::new(PlayerConfig::default(), OfflineConfig::default());
    harness.player().insert(make_resource(0.1), None);
    harness.player().insert(make_resource(0.1), None);

    harness.player().select_item(0, true).unwrap();

    let rendered = harness.render_until(|buf, _| buf.len() >= 8_820, 9_000);

    assert!(!rendered.is_empty());
    assert!(rendered.iter().any(|sample| sample.abs() > 0.0));
}
