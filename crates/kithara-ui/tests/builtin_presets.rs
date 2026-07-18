mod common;

use kithara_test_utils::kithara;
use kithara_ui::{builtin, compile::compile, source::Limits};

#[kithara::test]
fn micro_preset_compiles_against_player_registry() {
    compile(
        builtin::MICRO_PRESET,
        &builtin::resolver(),
        &common::player_catalog(),
        &common::player_registry(&["a"]),
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
        &common::player_registry(&["a"]),
        &Limits::default(),
    )
    .unwrap();
}
