use kithara_test_utils::kithara;

use super::*;

fn visible_tree_row_selected(reads: &MockReads, label: &str) -> bool {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter()
        .find(|row| row.label == label)
        .map(|row| row.selected)
        .unwrap_or_else(|| panic!("missing visible tree row {label}"))
}

fn visible_tree_row_index(reads: &MockReads, label: &str) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter()
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("missing visible tree row {label}"))
}

fn selected_visible_index(reads: &MockReads) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter()
        .position(|row| row.selected)
        .expect("a selected tree row")
}

fn muted_visible_index(reads: &MockReads) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.iter()
        .position(|row| row.muted)
        .expect("a muted tree row")
}

fn visible_tree_len(reads: &MockReads) -> usize {
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };
    rows.len()
}

#[kithara::test]
fn wave_scalar_write_updates_normalized_playback_position() {
    let mut reads = MockReads::default();

    reads.apply("modules/deck/wave", &ControlAction::SetScalar(0.25));

    assert_eq!(
        reads.get("deck.playback.position_normalized"),
        Some(ReadValue::Scalar(0.25))
    );
    assert_eq!(
        reads.get("deck.playback.position_secs"),
        Some(ReadValue::Scalar(Consts::DURATION_SECS * 0.25))
    );
}

#[kithara::test]
fn wave_zoom_is_host_owned_and_clamped() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("deck.view.zoom"),
        Some(ReadValue::Scalar(Consts::ZOOM))
    );
    reads.apply("modules/deck/wave/zoom", &ControlAction::SetScalar(0.001));
    assert_eq!(reads.get("deck.view.zoom"), Some(ReadValue::Scalar(0.015)));
    reads.apply("modules/deck/wave/zoom", &ControlAction::SetScalar(0.9));
    assert_eq!(reads.get("deck.view.zoom"), Some(ReadValue::Scalar(0.5)));
}

#[kithara::test]
fn deck_sync_and_reverse_toggles_update_active_reads() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("deck.playback.synced"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("deck.playback.reverse"),
        Some(ReadValue::Bool(false))
    );
    reads.apply("modules/deck/transport/sync", &ControlAction::Activate);
    reads.apply("modules/deck/transport/reverse", &ControlAction::Activate);
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
fn toggle_module_owns_the_collapsed_read_endpoint() {
    let mut reads = MockReads::default();
    let endpoint = "ui.module.gallery-module-deck.collapsed";

    assert_eq!(reads.get(endpoint), Some(ReadValue::Bool(false)));
    reads.toggle_module("gallery-module-deck".to_owned());
    assert_eq!(reads.get(endpoint), Some(ReadValue::Bool(true)));
    reads.toggle_module("gallery-module-deck".to_owned());
    assert_eq!(reads.get(endpoint), Some(ReadValue::Bool(false)));
}

#[kithara::test]
fn knob_gallery_values_cover_both_sides_of_center() {
    let mut reads = MockReads::default();

    assert_eq!(reads.get("mock.knob.26"), Some(ReadValue::Scalar(0.35)));
    assert_eq!(reads.get("mock.knob.28"), Some(ReadValue::Scalar(0.5)));
    assert_eq!(reads.get("mock.knob.34"), Some(ReadValue::Scalar(0.65)));
    assert_eq!(reads.get("mock.knob.38"), Some(ReadValue::Scalar(0.8)));

    reads.apply("atoms/knobs/size-26", &ControlAction::SetScalar(0.45));
    assert_eq!(reads.get("mock.knob.26"), Some(ReadValue::Scalar(0.45)));
    assert_eq!(reads.get("mock.knob.38"), Some(ReadValue::Scalar(0.8)));
}

#[kithara::test]
fn segmented_gallery_selects_an_index() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("mock.cells.segmented"),
        Some(ReadValue::Scalar(2.0))
    );
    reads.apply("cells/beat", &ControlAction::SelectIndex(3));
    assert_eq!(
        reads.get("mock.cells.segmented"),
        Some(ReadValue::Scalar(3.0))
    );
}

#[kithara::test]
fn vis_previous_and_next_cycle_the_preset() {
    let mut reads = MockReads::default();

    assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(0.0)));
    assert_eq!(
        reads.get("vis.preset_index"),
        Some(ReadValue::Text("1 / 3"))
    );

    reads.apply("vis/previous", &ControlAction::Activate);
    assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(2.0)));
    assert_eq!(
        reads.get("vis.preset_index"),
        Some(ReadValue::Text("3 / 3"))
    );

    reads.apply("vis/next", &ControlAction::Activate);
    assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(0.0)));
    reads.apply("vis/shader", &ControlAction::SelectIndex(1));
    assert_eq!(reads.get("vis.preset"), Some(ReadValue::Scalar(1.0)));
}

#[kithara::test]
fn tracklist_presets_replace_host_owned_column_visibility() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("gallery.tracklist.columns.energy"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("gallery.tracklist.columns.artist"),
        Some(ReadValue::Bool(false))
    );

    reads.apply("tracklist/column-preset", &ControlAction::SelectIndex(0));

    assert_eq!(
        reads.get("gallery.tracklist.columns.energy"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("gallery.tracklist.columns.artist"),
        Some(ReadValue::Bool(true))
    );
}

#[kithara::test]
fn tracklist_reset_restores_current_preset_defaults() {
    let mut reads = MockReads::default();

    reads.apply("tracklist/column-energy", &ControlAction::Activate);
    assert_eq!(
        reads.get("gallery.tracklist.columns.energy"),
        Some(ReadValue::Bool(false))
    );

    reads.apply("tracklist/reset-columns", &ControlAction::Activate);

    assert_eq!(
        reads.get("gallery.tracklist.columns.energy"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("gallery.tracklist.preset"),
        Some(ReadValue::Scalar(1.0))
    );
}

#[kithara::test]
fn tracklist_width_write_is_host_owned_and_clamped() {
    let mut reads = MockReads::default();
    let endpoint = "gallery.tracklist.columns.width.artist";

    assert_eq!(reads.get(endpoint), None);
    reads.apply(
        "tracklist/table/width/artist",
        &ControlAction::SetScalar(240.0),
    );
    assert_eq!(reads.get(endpoint), Some(ReadValue::Scalar(240.0)));

    reads.apply(
        "tracklist/table/width/artist",
        &ControlAction::SetScalar(1.0),
    );
    assert_eq!(
        reads.get(endpoint),
        Some(ReadValue::Scalar(f64::from(
            builtin::skin().track_list.min_column_width
        )))
    );
}

#[kithara::test]
fn tree_branch_selection_toggles_visible_descendants() {
    let mut reads = MockReads::default();
    let before = visible_tree_len(&reads);
    let explorer = visible_tree_row_index(&reads, "Explorer");

    reads.apply("tree/browser", &ControlAction::SelectIndex(explorer));
    let collapsed = visible_tree_len(&reads);
    assert!(collapsed < before);

    reads.apply("tree/browser", &ControlAction::SelectIndex(explorer));
    assert_eq!(visible_tree_len(&reads), before);
}

#[kithara::test]
fn tree_leaf_selection_is_host_owned() {
    let mut reads = MockReads::default();
    let previous = selected_visible_index(&reads);
    let all_tracks = visible_tree_row_index(&reads, "All tracks");
    assert_ne!(previous, all_tracks);

    reads.apply("library2/browser", &ControlAction::SelectIndex(all_tracks));

    assert!(visible_tree_row_selected(&reads, "All tracks"));
    assert_eq!(selected_visible_index(&reads), all_tracks);
}

#[kithara::test]
fn muted_tree_row_does_not_change_selection() {
    let mut reads = MockReads::default();
    let muted = muted_visible_index(&reads);
    let selected = selected_visible_index(&reads);

    reads.apply("tree/browser", &ControlAction::SelectIndex(muted));

    assert_eq!(selected_visible_index(&reads), selected);
}

#[kithara::test]
fn expanded_mock_tree_overflows_the_gallery_viewport() {
    let reads = MockReads::default();
    let Some(ReadValue::Tree(rows)) = reads.get("library.tree") else {
        panic!("expected tree rows");
    };

    assert!(rows.len() >= 30, "visible rows: {}", rows.len());
}

#[kithara::test]
fn library_query_read_reflects_host_updates() {
    let mut reads = MockReads::default();

    reads.set_library_query("acid bass".to_owned());

    assert_eq!(
        reads.get("library.query"),
        Some(ReadValue::Text("acid bass"))
    );
}

#[kithara::test]
fn context_scope_selection_is_host_owned() {
    let mut reads = MockReads::default();

    assert_eq!(reads.get("library.scope"), Some(ReadValue::Scalar(0.0)));
    reads.apply("library2/context", &ControlAction::SelectIndex(1));
    assert_eq!(reads.get("library.scope"), Some(ReadValue::Scalar(1.0)));
}

#[kithara::test]
fn default_menu_state_is_the_frozen_design_snapshot() {
    let reads = MockReads::default();

    assert_eq!(reads.get("ui.menu.open"), Some(ReadValue::Bool(true)));
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
fn the_popover_path_only_closes_while_the_burger_toggles() {
    let mut reads = MockReads::default();

    reads.apply("app-menu/pop", &ControlAction::Activate);
    assert_eq!(reads.get("ui.menu.open"), Some(ReadValue::Bool(false)));
    reads.apply("app-menu/pop", &ControlAction::Activate);
    assert_eq!(reads.get("ui.menu.open"), Some(ReadValue::Bool(false)));

    reads.apply("app-menu/burger", &ControlAction::Activate);
    assert_eq!(reads.get("ui.menu.open"), Some(ReadValue::Bool(true)));
    reads.apply("app-menu/burger", &ControlAction::Activate);
    assert_eq!(reads.get("ui.menu.open"), Some(ReadValue::Bool(false)));

    reads.apply("app-menu/burger", &ControlAction::Activate);
    reads.apply("app-menu/header-close", &ControlAction::Activate);
    assert_eq!(reads.get("ui.menu.open"), Some(ReadValue::Bool(false)));
}

#[kithara::test]
fn the_window_list_refuses_a_fourth_window_and_never_closes_the_first() {
    let mut reads = MockReads::default();

    assert_eq!(reads.get("ui.window.can_open"), Some(ReadValue::Bool(true)));
    assert_eq!(
        reads.get("ui.window.hidden@window=3"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.window.close_hidden@window=1"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.window.close_hidden@window=2"),
        Some(ReadValue::Bool(false))
    );

    reads.apply("app-menu/new-window", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.window.count"),
        Some(ReadValue::Text("3 WINDOWS"))
    );
    assert_eq!(
        reads.get("ui.window.can_open"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("ui.window.hidden@window=3"),
        Some(ReadValue::Bool(false))
    );

    reads.apply("app-menu/new-window", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.window.count"),
        Some(ReadValue::Text("3 WINDOWS"))
    );

    reads.apply("app-menu/window-1/close", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.window.count"),
        Some(ReadValue::Text("3 WINDOWS"))
    );

    reads.apply("app-menu/window-3/close", &ControlAction::Activate);
    reads.apply("app-menu/window-2/close", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.window.count"),
        Some(ReadValue::Text("1 WINDOW"))
    );
    assert_eq!(
        reads.get("ui.window.close_hidden@window=2"),
        Some(ReadValue::Bool(true))
    );
}

#[kithara::test]
fn focusing_a_window_moves_the_module_grid_and_the_layout_hint() {
    let mut reads = MockReads::default();

    reads.apply("app-menu/window-2/focus", &ControlAction::Activate);

    assert_eq!(
        reads.get("ui.window.active@window=2"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.modules.title"),
        Some(ReadValue::Text("Modules · WINDOW 2"))
    );
    assert_eq!(
        reads.get("ui.modules.count"),
        Some(ReadValue::Text("2 OF 11"))
    );
    assert_eq!(
        reads.get("ui.layouts.active"),
        Some(ReadValue::Text("VISUALS + TIMELINE"))
    );
    assert_eq!(
        reads.get("ui.module.on@module=vis"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.module.on@module=ov"),
        Some(ReadValue::Bool(false))
    );

    reads.apply("app-menu/module-ov/cell", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.module.on@module=ov"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.window.caption@window=2"),
        Some(ReadValue::Text("DELL U2720Q · 3 MOD."))
    );
}

#[kithara::test]
fn opening_one_menu_group_closes_the_other() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("ui.menu.group_hidden@group=mod"),
        Some(ReadValue::Bool(true))
    );
    reads.apply("app-menu/modules-head", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.menu.group_open@group=mod"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.menu.group_hidden@group=lay"),
        Some(ReadValue::Bool(true))
    );

    reads.apply("app-menu/layouts-head", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.menu.group_hidden@group=mod"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.menu.group_open@group=lay"),
        Some(ReadValue::Bool(true))
    );

    reads.apply("app-menu/layouts-head", &ControlAction::Activate);
    assert_eq!(
        reads.get("ui.menu.group_hidden@group=lay"),
        Some(ReadValue::Bool(true))
    );
}

#[kithara::test]
fn applying_a_layout_renames_the_active_window() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("ui.layout.selected@layout=1"),
        Some(ReadValue::Bool(true))
    );
    reads.apply("app-menu/layout-2/apply", &ControlAction::Activate);

    assert_eq!(
        reads.get("ui.layout.selected@layout=1"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("ui.layout.selected@layout=2"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("ui.window.title@window=1"),
        Some(ReadValue::Text("WINDOW 1 · STUDIO · 4 DECKS + VST"))
    );
    assert_eq!(
        reads.get("ui.layouts.active"),
        Some(ReadValue::Text("STUDIO · 4 DECKS + VST"))
    );
}

#[kithara::test]
fn the_record_and_cast_hints_follow_their_own_flags() {
    let mut reads = MockReads::default();

    assert_eq!(reads.get("ui.set.record_hint"), Some(ReadValue::Text("⌘R")));
    assert_eq!(
        reads.get("ui.set.cast_hint"),
        Some(ReadValue::Text("AUDIO LIVE"))
    );

    reads.apply("app-menu/record/toggle", &ControlAction::Activate);
    reads.apply("app-menu/cast/toggle", &ControlAction::Activate);

    assert_eq!(
        reads.get("ui.set.record_hint"),
        Some(ReadValue::Text("RECORDING"))
    );
    assert_eq!(reads.get("ui.set.cast_hint"), Some(ReadValue::Text("OFF")));
}

#[kithara::test]
fn a_secondary_click_opens_one_track_menu_at_a_time() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("gallery.menu.context@row=2"),
        Some(ReadValue::Bool(false))
    );
    reads.apply("ctx/track-2/row", &ControlAction::SecondaryActivate);
    assert_eq!(
        reads.get("gallery.menu.context@row=2"),
        Some(ReadValue::Bool(true))
    );

    reads.apply("ctx/track-1/row", &ControlAction::SecondaryActivate);
    assert_eq!(
        reads.get("gallery.menu.context@row=2"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("gallery.menu.context@row=1"),
        Some(ReadValue::Bool(true))
    );

    reads.apply("ctx/track-1/menu", &ControlAction::Activate);
    assert_eq!(
        reads.get("gallery.menu.context@row=1"),
        Some(ReadValue::Bool(false))
    );
}

#[kithara::test]
fn a_primary_click_selects_a_track_without_opening_its_menu() {
    let mut reads = MockReads::default();

    reads.apply("ctx/track-3/row", &ControlAction::Activate);

    assert_eq!(
        reads.get("gallery.menu.selected@row=3"),
        Some(ReadValue::Bool(true))
    );
    assert_eq!(
        reads.get("gallery.menu.selected@row=1"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("gallery.menu.context@row=3"),
        Some(ReadValue::Bool(false))
    );
}

#[kithara::test]
fn a_track_menu_action_closes_the_menu_and_reports_itself() {
    let mut reads = MockReads::default();

    reads.apply("ctx/track-2/row", &ControlAction::SecondaryActivate);
    reads.apply("ctx/track-2/deck-b", &ControlAction::Activate);

    assert_eq!(
        reads.get("gallery.menu.context@row=2"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        reads.get("gallery.menu.action"),
        Some(ReadValue::Text("DECK B · 2"))
    );

    reads.apply("ctx/track-4/row", &ControlAction::SecondaryActivate);
    reads.apply("ctx/track-4/queue", &ControlAction::Activate);
    assert_eq!(
        reads.get("gallery.menu.action"),
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
    let mut reads = MockReads::default();

    reads.apply(
        "clock/master-clock/source-c/select",
        &ControlAction::Activate,
    );
    assert_eq!(reads.get("clock.source"), Some(ReadValue::Text("C")));
    assert_eq!(
        reads.get("clock.source.active@source=c"),
        Some(ReadValue::Bool(true))
    );

    reads.apply("clock/master-clock/nudge/up", &ControlAction::Activate);
    assert_eq!(reads.get("clock.bpm"), Some(ReadValue::Text("124.01")));
    reads.apply("clock/master-clock/grid/click", &ControlAction::Activate);
    assert_eq!(reads.get("clock.grid.click"), Some(ReadValue::Bool(true)));
    reads.apply("clock/key-lock/toggle", &ControlAction::Activate);
    assert_eq!(
        reads.get("deck.key.locked@deck=a"),
        Some(ReadValue::Bool(false))
    );
    reads.apply(
        "clock/master-clock/link-panel/link-toggle",
        &ControlAction::Activate,
    );
    assert_eq!(
        reads.get("clock.link.enabled"),
        Some(ReadValue::Bool(false))
    );
    reads.apply(
        "clock/master-clock/midi-panel/midi-send",
        &ControlAction::Activate,
    );
    assert_eq!(reads.get("clock.midi.send"), Some(ReadValue::Bool(true)));
}

#[kithara::test]
fn pivot_controls_follow_the_handoff_ratio_range_and_loop_contract() {
    let mut reads = MockReads::default();

    assert_eq!(
        reads.get("pivot.selected.ratio"),
        Some(ReadValue::Text("4:3"))
    );
    assert_eq!(
        reads.get("pivot.selected.target"),
        Some(ReadValue::Text("93.00"))
    );
    assert_eq!(
        reads.get("pivot.track.title@track=0"),
        Some(ReadValue::Text("slowtechno_mas-5"))
    );
    let Some(ReadValue::PortalMap(map)) = reads.get("pivot.map") else {
        panic!("expected portal map");
    };
    assert_eq!(map.master, 124.0);
    assert!(map.targets.iter().any(|target| target.is_selected));

    reads.apply(
        "pivot/table/portal-scroll/portal-list/row-0/select",
        &ControlAction::Activate,
    );
    assert_eq!(
        reads.get("pivot.selected.ratio"),
        Some(ReadValue::Text("12:11"))
    );
    assert_eq!(
        reads.get("pivot.portal.active@portal=0"),
        Some(ReadValue::Bool(true))
    );

    reads.apply("pivot/range/min", &ControlAction::SetScalar(1.0));
    assert_eq!(
        reads.get("pivot.range.min_label"),
        Some(ReadValue::Text("168"))
    );
    let Some(ReadValue::Range(range)) = reads.get("pivot.range") else {
        panic!("expected range");
    };
    assert!(((range.max - range.min) - 8.0 / 140.0).abs() < f32::EPSILON);

    reads.apply("pivot/family/leap", &ControlAction::Activate);
    assert_eq!(reads.get("pivot.family.leap"), Some(ReadValue::Bool(true)));

    reads.apply("pivot/loops/mul-4", &ControlAction::Activate);
    assert_eq!(reads.get("pivot.multiplier.4"), Some(ReadValue::Bool(true)));
}
