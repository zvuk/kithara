#![cfg(all(feature = "perf", feature = "iced", feature = "masonry"))]

use kithara_test_utils::kithara;
use kithara_ui::expand::ControlSpec;

mod scenarios;
mod types;
use types::{Interaction, Scenario};

use crate::scenarios::consts;

#[kithara::test]
fn every_document_control_has_a_frame_scenario() {
    assert!(consts::WIDTH > 0 && consts::HEIGHT > 0);
    assert!(!consts::LAYOUT.is_empty());
    for scenario in consts::SCENARIOS {
        assert!(!scenario.control.is_empty());
        for step in scenario.interaction.steps(kithara_ui::draw::Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        }) {
            if let types::Step::Move(at) = step {
                assert!(at.x.is_finite() && at.y.is_finite());
            }
        }
    }
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
    for scenario in consts::SCENARIOS {
        assert!(!scenario.control.is_empty());
        for step in scenario.interaction.steps(kithara_ui::draw::Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        }) {
            if let types::Step::Move(at) = step {
                assert!(at.x.is_finite() && at.y.is_finite());
            }
        }
    }
    let unknown: Vec<_> = consts::SCENARIOS
        .iter()
        .map(|scenario| scenario.name)
        .filter(|name| !ControlSpec::KINDS.contains(name))
        .collect();
    assert!(unknown.is_empty(), "unknown scenarios: {unknown:?}");
}
