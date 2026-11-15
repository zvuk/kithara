//! What the gallery's readings answer, and how a press changes them.
//!
//! The demo model behind the pages is the application the gallery is; these
//! are the questions asked of it, which belong with the gallery's other
//! checks rather than beside the model itself.
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    render::{ControlAction, ReadValue, Reads as _, Zoom},
};

use super::Hand;
use crate::demo::{DemoReads, consts, data::CATALOG};

fn visible_tree_row_selected(reads: &DemoReads, label: &str) -> bool {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter().find(|row| row.label == label).map_or_else(
        || panic!("missing visible tree row {label}"),
        |row| row.selected,
    )
}

fn visible_tree_row_index(reads: &DemoReads, label: &str) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter()
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("missing visible tree row {label}"))
}

fn selected_visible_index(reads: &DemoReads) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter()
        .position(|row| row.selected)
        .expect("a selected tree row")
}

fn muted_visible_index(reads: &DemoReads) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter()
        .position(|row| row.muted)
        .expect("a muted tree row")
}

fn visible_tree_len(reads: &DemoReads) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.len()
}

#[kithara::test]
fn wave_scalar_write_updates_normalized_playback_position() {
    let mut hand = Hand::at("modules");

    hand.gesture("modules/deck/wave", ControlAction::SetScalar(0.25));

    let reads = &hand.reads;
    assert_eq!(
        reads.get("deck.playback.position_normalized"),
        Some(ReadValue::Scalar(0.25))
    );
    assert_eq!(
        reads.get("deck.playback.position_secs"),
        Some(ReadValue::Scalar(consts::DURATION_SECS * 0.25))
    );
}

#[kithara::test]
fn wave_zoom_is_host_owned_and_clamped() {
    let mut hand = Hand::at("modules");

    assert_eq!(
        hand.reads.get("deck.view.zoom"),
        Some(ReadValue::Scalar(f64::from(f32::from(Zoom::DEFAULT))))
    );
    hand.gesture("modules/deck/wave/zoom", ControlAction::SetScalar(0.001));
    assert_eq!(
        hand.reads.get("deck.view.zoom"),
        Some(ReadValue::Scalar(f64::from(f32::from(Zoom::MIN))))
    );
    hand.gesture("modules/deck/wave/zoom", ControlAction::SetScalar(0.9));
    assert_eq!(
        hand.reads.get("deck.view.zoom"),
        Some(ReadValue::Scalar(0.5))
    );
}

#[kithara::test]
fn deck_sync_and_reverse_toggles_update_active_reads() {
    let mut hand = Hand::at("modules");

    assert_eq!(
        hand.reads.get("deck.playback.synced"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("deck.playback.reverse"),
        Some(ReadValue::Bool(false))
    );
    hand.press("modules/deck/transport/sync");
    hand.press("modules/deck/transport/reverse");
    let reads = &hand.reads;
    assert_eq!(
        reads.get("deck.playback.synced"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("deck.playback.reverse"),
        Some(ReadValue::Bool(true))
    );
}

#[kithara::test]
fn pressing_a_module_header_folds_and_unfolds_that_module() {
    let mut hand = Hand::at_module("deck");
    let endpoint = "ui.module.gallery-module-deck.collapsed";

    assert_eq!(hand.reads.get(endpoint), Some(ReadValue::Bool(false)));
    hand.press("modules/header");
    assert_eq!(hand.reads.get(endpoint), Some(ReadValue::Bool(true)));
    hand.press("modules/header");
    assert_eq!(hand.reads.get(endpoint), Some(ReadValue::Bool(false)));
}

#[kithara::test]
fn knob_gallery_values_cover_both_sides_of_center() {
    let mut hand = Hand::at("atoms");
    let reads = &hand.reads;

    assert_eq!(reads.get("demo.knob.26"), Some(ReadValue::Scalar(0.35)));
    assert_eq!(reads.get("demo.knob.28"), Some(ReadValue::Scalar(0.5)));
    assert_eq!(reads.get("demo.knob.34"), Some(ReadValue::Scalar(0.65)));
    assert_eq!(reads.get("demo.knob.38"), Some(ReadValue::Scalar(0.8)));

    hand.gesture("atoms/knobs/size-26", ControlAction::SetScalar(0.45));
    let reads = &hand.reads;
    assert_eq!(reads.get("demo.knob.26"), Some(ReadValue::Scalar(0.45)));
    assert_eq!(reads.get("demo.knob.38"), Some(ReadValue::Scalar(0.8)));
}

#[kithara::test]
fn segmented_gallery_selects_an_index() {
    let mut hand = Hand::at("cells");

    assert_eq!(
        hand.reads.get("demo.cells.segmented"),
        Some(ReadValue::Scalar(2.0))
    );
    hand.gesture("cells/beat", ControlAction::SelectIndex(3));
    assert_eq!(
        hand.reads.get("demo.cells.segmented"),
        Some(ReadValue::Scalar(3.0))
    );
}

#[kithara::test]
fn vis_previous_and_next_cycle_the_preset() {
    let mut hand = Hand::at("vis");

    assert_eq!(hand.reads.get("vis.preset"), Some(ReadValue::Scalar(0.0)));
    assert_eq!(
        hand.reads.get("vis.preset_index"),
        Some(ReadValue::Text("1 / 3"))
    );

    hand.press("vis/previous");
    assert_eq!(hand.reads.get("vis.preset"), Some(ReadValue::Scalar(2.0)));
    assert_eq!(
        hand.reads.get("vis.preset_index"),
        Some(ReadValue::Text("3 / 3"))
    );

    hand.press("vis/next");
    assert_eq!(hand.reads.get("vis.preset"), Some(ReadValue::Scalar(0.0)));
}

#[kithara::test]
fn table_presets_replace_host_owned_column_visibility() {
    let mut hand = Hand::at("table");

    assert_eq!(
        hand.reads.get("gallery.table.columns.energy"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("gallery.table.columns.artist"),
        Some(ReadValue::Bool(false))
    );

    hand.gesture("table/column-preset", ControlAction::SelectIndex(0));

    let reads = &hand.reads;
    assert_eq!(
        reads.get("gallery.table.columns.energy"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("gallery.table.columns.artist"),
        Some(ReadValue::Bool(true))
    );
}

#[kithara::test]
fn table_reset_restores_current_preset_defaults() {
    let mut hand = Hand::at("table");

    hand.press("table/column-energy");
    assert_eq!(
        hand.reads.get("gallery.table.columns.energy"),
        Some(ReadValue::Bool(false))
    );

    hand.press("table/reset-columns");

    let reads = &hand.reads;
    assert_eq!(
        reads.get("gallery.table.columns.energy"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("gallery.table.preset"),
        Some(ReadValue::Scalar(1.0))
    );
}

#[kithara::test]
fn table_width_write_is_host_owned_and_clamped() {
    let mut hand = Hand::at("table");
    let endpoint = "gallery.table.width@column=artist";

    assert_eq!(hand.reads.get(endpoint), None);
    hand.gesture("table/table/width/artist", ControlAction::SetScalar(240.0));
    assert_eq!(hand.reads.get(endpoint), Some(ReadValue::Scalar(240.0)));

    hand.gesture("table/table/width/artist", ControlAction::SetScalar(1.0));
    assert_eq!(
        hand.reads.get(endpoint),
        Some(ReadValue::Scalar(f64::from(
            builtin::skin().table.min_column_width
        )))
    );
}

#[kithara::test]
fn tree_branch_selection_toggles_visible_descendants() {
    let mut hand = Hand::at("tree");
    let before = visible_tree_len(&hand.reads);
    let explorer = visible_tree_row_index(&hand.reads, "Explorer");

    hand.gesture("tree/browser", ControlAction::SelectIndex(explorer));
    let collapsed = visible_tree_len(&hand.reads);
    assert!(collapsed < before);

    hand.gesture("tree/browser", ControlAction::SelectIndex(explorer));
    assert_eq!(visible_tree_len(&hand.reads), before);
}

#[kithara::test]
fn tree_leaf_selection_is_host_owned() {
    let mut hand = Hand::at("library2");
    let previous = selected_visible_index(&hand.reads);
    let all_tracks = visible_tree_row_index(&hand.reads, "All tracks");
    assert_ne!(previous, all_tracks);

    hand.gesture("library2/browser", ControlAction::SelectIndex(all_tracks));

    let reads = &hand.reads;
    assert!(visible_tree_row_selected(reads, "All tracks"));
    assert_eq!(selected_visible_index(reads), all_tracks);
}

#[kithara::test]
fn muted_tree_row_does_not_change_selection() {
    let mut hand = Hand::at("tree");
    let muted = muted_visible_index(&hand.reads);
    let selected = selected_visible_index(&hand.reads);

    hand.gesture("tree/browser", ControlAction::SelectIndex(muted));

    assert_eq!(selected_visible_index(&hand.reads), selected);
}

#[kithara::test]
fn expanded_demo_tree_overflows_the_gallery_viewport() {
    let reads = DemoReads::default();
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };

    assert!(rows.len() >= 30, "visible rows: {}", rows.len());
}

#[kithara::test]
fn typing_in_the_tree_search_is_the_query_it_reads() {
    let mut hand = Hand::at("tree");

    hand.gesture(
        "tree/browser/search",
        ControlAction::Text("acid bass".to_owned()),
    );

    assert_eq!(
        hand.reads.get("library.query"),
        Some(ReadValue::Text("acid bass"))
    );
}

#[kithara::test]
fn context_scope_selection_is_host_owned() {
    let mut hand = Hand::at("library2");

    assert_eq!(
        hand.reads.get("library.scope"),
        Some(ReadValue::Scalar(0.0))
    );
    hand.gesture("library2/context", ControlAction::SelectIndex(1));
    assert_eq!(
        hand.reads.get("library.scope"),
        Some(ReadValue::Scalar(1.0))
    );
}

#[kithara::test]
fn default_menu_state_is_the_frozen_design_snapshot() {
    let reads = DemoReads::default();

    assert_eq!(
        reads.get("ui.window.count"),
        Some(ReadValue::Text("2 WINDOWS"))
    );
    assert_eq!(
        reads.get("ui.window.title@window=1"),
        Some(ReadValue::Text("WINDOW 1 · CLUB · 2 DECKS"))
    );
    assert_eq!(
        reads.get("ui.window.caption@window=1"),
        Some(ReadValue::Text("MACBOOK PRO 16\" · 8 MOD."))
    );
    assert_eq!(
        reads.get("ui.window.title@window=2"),
        Some(ReadValue::Text("WINDOW 2 · VISUALS + TIMELINE"))
    );
    assert_eq!(
        reads.get("ui.window.caption@window=2"),
        Some(ReadValue::Text("DELL U2720Q · 2 MOD."))
    );
    assert_eq!(
        reads.get("ui.modules.title"),
        Some(ReadValue::Text("Modules · WINDOW 1"))
    );
    assert_eq!(
        reads.get("ui.modules.count"),
        Some(ReadValue::Text("8 OF 11"))
    );
    assert_eq!(
        reads.get("ui.layouts.active"),
        Some(ReadValue::Text("CLUB · 2 DECKS"))
    );
    assert_eq!(
        reads.get("ui.prefs.wave_follow"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(reads.get("ui.prefs.autogain"), Some(ReadValue::Bool(true)));
    assert_eq!(reads.get("ui.prefs.mono"), Some(ReadValue::Bool(false)));
    assert_eq!(reads.get("ui.set.recording"), Some(ReadValue::Bool(false)));
    assert_eq!(reads.get("ui.set.casting"), Some(ReadValue::Bool(true)));
}

#[kithara::test]
fn the_window_list_refuses_a_fourth_window_and_never_closes_the_first() {
    let mut hand = Hand::at("menu");

    assert_eq!(
        hand.reads.get("ui.window.can_open"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("ui.window.hidden@window=3"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("ui.window.close_hidden@window=1"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("ui.window.close_hidden@window=2"),
        Some(ReadValue::Bool(false))
    );

    hand.press("app-menu/menu/new-window");
    assert_eq!(
        hand.reads.get("ui.window.count"),
        Some(ReadValue::Text("3 WINDOWS"))
    );
    assert_eq!(
        hand.reads.get("ui.window.can_open"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        hand.reads.get("ui.window.hidden@window=3"),
        Some(ReadValue::Bool(false))
    );

    hand.press("app-menu/menu/new-window");
    assert_eq!(
        hand.reads.get("ui.window.count"),
        Some(ReadValue::Text("3 WINDOWS"))
    );

    hand.press("app-menu/menu/window-1/close");
    assert_eq!(
        hand.reads.get("ui.window.count"),
        Some(ReadValue::Text("3 WINDOWS"))
    );

    hand.press("app-menu/menu/window-3/close");
    hand.press("app-menu/menu/window-2/close");
    assert_eq!(
        hand.reads.get("ui.window.count"),
        Some(ReadValue::Text("1 WINDOW"))
    );
    assert_eq!(
        hand.reads.get("ui.window.close_hidden@window=2"),
        Some(ReadValue::Bool(true))
    );
}

#[kithara::test]
fn focusing_a_window_moves_the_module_grid_and_the_layout_hint() {
    let mut hand = Hand::at("menu");

    hand.press("app-menu/menu/window-2/focus");

    assert_eq!(
        hand.reads.get("ui.window.active@window=2"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("ui.modules.title"),
        Some(ReadValue::Text("Modules · WINDOW 2"))
    );
    assert_eq!(
        hand.reads.get("ui.modules.count"),
        Some(ReadValue::Text("2 OF 11"))
    );
    assert_eq!(
        hand.reads.get("ui.layouts.active"),
        Some(ReadValue::Text("VISUALS + TIMELINE"))
    );
    assert_eq!(
        hand.reads.get("ui.module.on@module=vis"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("ui.module.on@module=ov"),
        Some(ReadValue::Bool(false))
    );

    hand.press("app-menu/menu/module-ov/cell");
    assert_eq!(
        hand.reads.get("ui.module.on@module=ov"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("ui.window.caption@window=2"),
        Some(ReadValue::Text("DELL U2720Q · 3 MOD."))
    );
}

#[kithara::test]
fn a_menu_group_head_opens_and_folds_its_group() {
    const GROUP: &str = "app-menu/group-mod";
    let mut hand = Hand::at("menu");

    assert!(!hand.view.flag(GROUP));
    hand.press("app-menu/menu/modules-head");
    assert!(hand.view.flag(GROUP));
    hand.press("app-menu/menu/modules-head");
    assert!(!hand.view.flag(GROUP));
}

#[kithara::test]
fn applying_a_layout_renames_the_active_window() {
    let mut hand = Hand::at("menu");

    assert_eq!(
        hand.reads.get("ui.layout.selected@layout=1"),
        Some(ReadValue::Bool(true))
    );
    hand.press("app-menu/menu/layout-2/apply");

    assert_eq!(
        hand.reads.get("ui.layout.selected@layout=1"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        hand.reads.get("ui.layout.selected@layout=2"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("ui.window.title@window=1"),
        Some(ReadValue::Text("WINDOW 1 · STUDIO · 4 DECKS + VST"))
    );
    assert_eq!(
        hand.reads.get("ui.layouts.active"),
        Some(ReadValue::Text("STUDIO · 4 DECKS + VST"))
    );
}

#[kithara::test]
fn the_record_and_cast_hints_follow_their_own_flags() {
    let mut hand = Hand::at("menu");

    assert_eq!(
        hand.reads.get("ui.set.record_hint"),
        Some(ReadValue::Text("\u{2318}R"))
    );
    assert_eq!(
        hand.reads.get("ui.set.cast_hint"),
        Some(ReadValue::Text("AUDIO LIVE"))
    );

    hand.press("app-menu/menu/record/toggle");
    hand.press("app-menu/menu/cast/toggle");

    assert_eq!(
        hand.reads.get("ui.set.record_hint"),
        Some(ReadValue::Text("RECORDING"))
    );
    assert_eq!(
        hand.reads.get("ui.set.cast_hint"),
        Some(ReadValue::Text("OFF"))
    );
}

#[kithara::test]
fn a_secondary_click_opens_that_track_menu_and_dismissing_shuts_it() {
    let mut hand = Hand::at("menu");

    assert!(!hand.view.flag("ctx/2"));
    hand.gesture("ctx/track-2/row", ControlAction::SecondaryActivate);
    assert!(hand.view.flag("ctx/2"));
    assert!(!hand.view.flag("ctx/1"));

    hand.press("ctx/track-2/menu");
    assert!(!hand.view.flag("ctx/2"));
}

#[kithara::test]
fn a_primary_click_selects_a_track_without_opening_its_menu() {
    let mut hand = Hand::at("menu");

    hand.press("ctx/track-3/row");

    assert_eq!(
        hand.reads.get("gallery.menu.selected@row=3"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        hand.reads.get("gallery.menu.selected@row=1"),
        Some(ReadValue::Bool(false))
    );
    assert!(!hand.view.flag("ctx/3"));
}

#[kithara::test]
fn a_track_menu_action_closes_the_menu_and_reports_itself() {
    let mut hand = Hand::at("menu");

    hand.gesture("ctx/track-2/row", ControlAction::SecondaryActivate);
    hand.press("ctx/track-2/deck-b");

    assert!(!hand.view.flag("ctx/2"));
    assert_eq!(
        hand.reads.get("gallery.menu.action"),
        Some(ReadValue::Text("DECK B · 2"))
    );

    hand.gesture("ctx/track-4/row", ControlAction::SecondaryActivate);
    hand.press("ctx/track-4/queue");
    assert_eq!(
        hand.reads.get("gallery.menu.action"),
        Some(ReadValue::Text("TO QUEUE · 4"))
    );
}

#[kithara::test]
fn breadcrumb_data_excludes_the_scope_prefix() {
    assert!(!CATALOG.breadcrumb.is_empty());
    assert!(!CATALOG.breadcrumb.contains('\u{203a}'));
}

#[kithara::test]
fn clock_controls_update_source_tempo_grid_and_key_lock() {
    let mut hand = Hand::at("clock");

    hand.press("clock-components/master-clock/surface/source-c/select");
    assert_eq!(hand.reads.get("clock.source"), Some(ReadValue::Text("C")));
    assert_eq!(
        hand.reads.get("clock.source.active@source=c"),
        Some(ReadValue::Bool(true))
    );

    hand.press("clock-components/master-clock/up");
    assert_eq!(hand.reads.get("clock.bpm"), Some(ReadValue::Text("124.01")));
    hand.press("clock-components/master-clock/surface/click");
    assert_eq!(
        hand.reads.get("clock.grid.click"),
        Some(ReadValue::Bool(true))
    );
    hand.press("clock-components/key-lock/chip/toggle");
    assert_eq!(
        hand.reads.get("deck.key.locked@deck=a"),
        Some(ReadValue::Bool(false))
    );
    hand.press("clock-components/master-clock/surface/link-toggle");
    assert_eq!(
        hand.reads.get("clock.link.enabled"),
        Some(ReadValue::Bool(false))
    );
    hand.press("clock-components/master-clock/surface/midi-send");
    assert_eq!(
        hand.reads.get("clock.midi.send"),
        Some(ReadValue::Bool(true))
    );
}

#[kithara::test]
fn a_panel_stays_open_through_the_writes_it_delivers() {
    for (page, open, presses) in [
        (
            "menu",
            "app-menu/menu",
            [
                "app-menu/menu/module-ov/cell",
                "app-menu/menu/module-mix/cell",
                "app-menu/menu/layout-2/apply",
            ],
        ),
        (
            "clock",
            "clock-components/clock",
            [
                "clock-components/master-clock/surface/tap",
                "clock-components/master-clock/surface/tap",
                "clock-components/master-clock/surface/source-c/select",
            ],
        ),
    ] {
        let mut hand = Hand::at(page);
        assert!(hand.view.flag(open), "`{page}` is shown with `{open}` open");

        for path in presses {
            hand.press(path);
            assert!(hand.view.flag(open), "`{path}` must leave `{open}` open");
        }
    }
}

#[kithara::test]
fn pivot_controls_follow_the_handoff_ratio_range_and_loop_contract() {
    let mut hand = Hand::at("pivot");

    assert_eq!(
        hand.reads.get("pivot.selected.ratio"),
        Some(ReadValue::Text("4:3"))
    );
    assert_eq!(
        hand.reads.get("pivot.selected.target"),
        Some(ReadValue::Text("93.00"))
    );
    assert_eq!(
        hand.reads.get("pivot.track.title@track=0"),
        Some(ReadValue::Text("slowtechno_mas-5"))
    );
    let Some(ReadValue::PortalMap(map)) = hand.reads.get("pivot.map") else {
        panic!("expected portal map");
    };
    assert_eq!(map.master, 124.0);
    assert!(map.targets.iter().any(|target| target.is_selected));

    hand.press("pivot/row-0/select");
    assert_eq!(
        hand.reads.get("pivot.selected.ratio"),
        Some(ReadValue::Text("12:11"))
    );
    assert_eq!(
        hand.reads.get("pivot.portal.active@portal=0"),
        Some(ReadValue::Bool(true))
    );

    hand.gesture("pivot/range/min", ControlAction::SetScalar(1.0));
    assert_eq!(
        hand.reads.get("pivot.range.min_label"),
        Some(ReadValue::Text("168"))
    );
    let Some(ReadValue::Range(range)) = hand.reads.get("pivot.range") else {
        panic!("expected range");
    };
    assert!(((range.max - range.min) - 8.0 / 140.0).abs() < f32::EPSILON);

    hand.press("pivot/leap");
    assert_eq!(
        hand.reads.get("pivot.family.leap"),
        Some(ReadValue::Bool(true))
    );

    hand.press("pivot/mul-4");
    assert_eq!(
        hand.reads.get("pivot.multiplier.4"),
        Some(ReadValue::Bool(true))
    );
}

/// The stress page's waveform length is the one weight of that page a
/// measurement can vary, so it has to follow the count it was built with rather
/// than a constant baked into the demo model.
#[kithara::test]
fn the_stress_waveform_carries_the_bucket_count_it_was_built_with() {
    let mut reads = DemoReads::default();
    for buckets in [8_192_u16, 256] {
        reads.set_wave_buckets(buckets);
        let Some(ReadValue::Waveform(view)) = reads.get("bench.wave.0") else {
            panic!("the stress page must answer a waveform");
        };
        assert_eq!(view.buckets.len(), usize::from(buckets));
    }
}
