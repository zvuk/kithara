mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    layout::Axis,
    source::Limits,
};

#[kithara::test]
fn micro_preset_compiles_against_player_registry() {
    compile(
        builtin::MICRO_PRESET,
        &builtin::resolver(),
        &common::player_catalog(),
        &common::player_registry(),
        &Limits::default(),
    )
    .unwrap();
}

#[kithara::test]
fn player_preset_compiles_against_player_registry() {
    compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_catalog(),
        &common::player_registry(),
        &Limits::default(),
    )
    .unwrap();
}

#[kithara::test]
fn player_preset_size_sums_vertical_module_heights() {
    let ui = compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_catalog(),
        &common::player_registry(),
        &Limits::default(),
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
        size: deck_size, ..
    } = &children[0].1
    else {
        panic!("expected deck module");
    };
    let CompiledNode::Module {
        size: library_size, ..
    } = &children[1].1
    else {
        panic!("expected library module");
    };

    assert_eq!(deck_size.h.min(), 3.0 * common::CONTROL_SIZE);
    assert_eq!(library_size.h.min(), common::CONTROL_SIZE);
    assert_eq!(ui.size.h.min(), deck_size.h.min() + library_size.h.min());
    assert_eq!(ui.size, *size);
}
