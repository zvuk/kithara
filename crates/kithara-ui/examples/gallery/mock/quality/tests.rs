use kithara_test_utils::kithara;

use super::*;

const CELL: &str = "modules/deck/transport/stream/cell";
const POP: &str = "modules/deck/transport/stream/pop";

fn text(state: &QualityState, endpoint: &str) -> String {
    match state.get(endpoint) {
        Some(ReadValue::Text(value)) => value.to_owned(),
        other => panic!("{endpoint} must read as text, got {other:?}"),
    }
}

fn flag(state: &QualityState, endpoint: &str) -> bool {
    match state.get(endpoint) {
        Some(ReadValue::Bool(value)) => value,
        other => panic!("{endpoint} must read as a flag, got {other:?}"),
    }
}

#[kithara::test]
fn the_cell_names_the_variant_the_ladder_plays_while_it_picks_them() {
    let state = QualityState::default();

    assert_eq!(text(&state, "deck.stream.quality@deck=a"), "AUTO·320");
    assert!(flag(
        &state,
        "deck.stream.variant_active@deck=a,variant=auto"
    ));
    assert!(!flag(&state, "deck.stream.variant_active@deck=a,variant=1"));
}

#[kithara::test]
fn picking_a_variant_leaves_auto_and_closes_the_menu() {
    let mut state = QualityState::default();
    state.activate(CELL);
    assert!(flag(&state, "deck.stream.quality_menu@deck=a"));

    state.activate("modules/deck/transport/stream/variant-2/pick");

    assert_eq!(text(&state, "deck.stream.quality@deck=a"), "128");
    assert!(!flag(
        &state,
        "deck.stream.variant_active@deck=a,variant=auto"
    ));
    assert!(flag(&state, "deck.stream.variant_active@deck=a,variant=2"));
    assert!(!flag(&state, "deck.stream.quality_menu@deck=a"));
}

#[kithara::test]
fn the_popover_path_closes_the_menu_and_the_cell_toggles_it() {
    let mut state = QualityState::default();

    state.activate(CELL);
    state.activate(POP);
    assert!(!flag(&state, "deck.stream.quality_menu@deck=a"));

    state.activate(CELL);
    state.activate(CELL);
    assert!(!flag(&state, "deck.stream.quality_menu@deck=a"));
}

#[kithara::test]
fn a_slot_beyond_the_ladder_reads_hidden() {
    let state = QualityState::default();

    assert!(!flag(&state, "deck.stream.variant_hidden@deck=a,variant=2"));
    assert!(flag(&state, "deck.stream.variant_hidden@deck=a,variant=3"));
}
