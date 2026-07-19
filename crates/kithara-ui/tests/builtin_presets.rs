mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    expand::ExpandedNode,
    layout::Axis,
    source::UiConfig,
};

#[kithara::test]
fn micro_preset_compiles_against_player_registry() {
    let ui = compile(
        builtin::MICRO_PRESET,
        &builtin::resolver(),
        &common::player_catalog(),
        &common::player_registry(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected micro module");
    };
    let ExpandedNode::Row { children, .. } = &**root else {
        panic!("micro must compile to one row");
    };
    let kinds: Vec<_> = children
        .iter()
        .filter_map(|child| match child {
            ExpandedNode::Control { kind, .. } => Some(ui.resolve(*kind)),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "button",
            "deck.summary",
            "telemetry.bpm",
            "fader.horizontal",
            "waveform.mini",
            "view.settings",
        ]
    );
}

#[kithara::test]
fn player_preset_compiles_against_player_registry() {
    compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_catalog(),
        &common::player_registry(),
        &UiConfig::default(),
    )
    .unwrap();
}

#[kithara::test]
fn player_preset_size_sums_global_deck_and_library_heights() {
    let ui = compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_catalog(),
        &common::player_registry(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Split {
        axis,
        children,
        size,
    } = &ui.root
    else {
        panic!("expected split root");
    };
    assert_eq!(*axis, Axis::Vertical);
    let CompiledNode::Module {
        size: global_size, ..
    } = &children[0].1
    else {
        panic!("expected global bar module");
    };
    let CompiledNode::Module {
        size: deck_size, ..
    } = &children[1].1
    else {
        panic!("expected deck module");
    };
    let CompiledNode::Module {
        size: library_size, ..
    } = &children[2].1
    else {
        panic!("expected library module");
    };

    assert_eq!(global_size.h.min(), 34.0);
    assert_eq!(deck_size.h.min(), 3.0 * common::CONTROL_SIZE);
    assert_eq!(library_size.h.min(), common::CONTROL_SIZE);
    assert_eq!(
        ui.size.h.min(),
        global_size.h.min() + deck_size.h.min() + library_size.h.min()
    );
    assert_eq!(ui.size, *size);
}
