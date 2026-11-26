#![cfg(all(feature = "perf", feature = "iced", feature = "masonry"))]

use kithara_test_utils::kithara;
use kithara_ui::expand::ControlSpec;

use crate::scenarios::consts;

#[kithara::test]
fn every_document_control_has_a_frame_scenario() {
    let missing: Vec<_> = ControlSpec::KINDS
        .iter()
        .copied()
        .filter(|kind| {
            !consts::SCENARIOS
                .iter()
                .any(|scenario| scenario.name == *kind)
        })
        .collect();
    assert!(missing.is_empty(), "unmeasured controls: {missing:?}");
}

#[kithara::test]
fn every_frame_scenario_is_a_document_control() {
    let unknown: Vec<_> = consts::SCENARIOS
        .iter()
        .map(|scenario| scenario.name)
        .filter(|name| !ControlSpec::KINDS.contains(name))
        .collect();
    assert!(unknown.is_empty(), "unknown scenarios: {unknown:?}");
}
