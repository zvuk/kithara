mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi, compile},
    expand::{Binding, BindingKind, ControlSpec, ExpandedNode},
    layout::Axis,
    module::{IconName, WaveStyle},
    size::Dim,
    source::{SourceResolver, UiConfig},
};

fn micro_preset() -> CompiledUi {
    compile(
        builtin::MICRO_PRESET,
        &builtin::resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .expect("the micro preset compiles")
}

/// The bar the micro preset stands at every height, above the blocks the
/// window takes as it grows.
fn micro_bar(ui: &CompiledUi) -> &ExpandedNode {
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("the micro preset is one module");
    };
    let ExpandedNode::Column { children, .. } = &**root else {
        panic!("the micro preset stacks its blocks");
    };
    children.first().expect("the bar stands at every height")
}

/// The direct cells of a bar, as `(band, address)` in document order. A cell
/// standing at every width names the band `(0.0, None)`.
fn bar_cells<'a>(ui: &'a CompiledUi, bar: &'a ExpandedNode) -> Vec<((f32, Option<f32>), &'a str)> {
    let ExpandedNode::Row { children, .. } = bar else {
        panic!("a bar is one row");
    };
    children
        .iter()
        .map(|child| match child {
            ExpandedNode::Reveal { from, until, child } => ((*from, *until), cell_id(ui, child)),
            child => ((0.0, None), cell_id(ui, child)),
        })
        .collect()
}

/// The blocks the micro preset stacks, as `(threshold, address)`.
fn micro_blocks(ui: &CompiledUi) -> Vec<((f32, Option<f32>), &str)> {
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("the micro preset is one module");
    };
    let ExpandedNode::Column { children, .. } = &**root else {
        panic!("the micro preset stacks its blocks");
    };
    children
        .iter()
        .map(|child| match child {
            ExpandedNode::Reveal { from, until, child } => ((*from, *until), cell_id(ui, child)),
            child => ((0.0, None), cell_id(ui, child)),
        })
        .collect()
}

fn cell_id<'a>(ui: &'a CompiledUi, node: &'a ExpandedNode) -> &'a str {
    match node {
        ExpandedNode::Control { path, .. }
        | ExpandedNode::Popover { path, .. }
        | ExpandedNode::Pressable { path, .. } => ui.resolve(*path),
        ExpandedNode::Reveal { child, .. } | ExpandedNode::Scroll { child, .. } => {
            cell_id(ui, child)
        }
        ExpandedNode::Row { children, .. } | ExpandedNode::Column { children, .. } => {
            children.first().map_or("", |child| cell_id(ui, child))
        }
        _ => "",
    }
}

/// The micro bar takes its cells as the window widens, at the widths the
/// prototype names.
#[kithara::test]
fn the_micro_bar_reveals_its_cells_as_the_window_widens() {
    let ui = micro_preset();

    assert_eq!(
        bar_cells(&ui, micro_bar(&ui)),
        vec![
            ((0.0, None), "deck-a/bar/menu/pop"),
            ((0.0, None), "deck-a/bar/play"),
            ((440.0, None), "deck-a/bar/summary"),
            ((0.0, Some(350.0)), "deck-a/bar/drag"),
            ((350.0, None), "deck-a/bar/wave"),
            ((520.0, None), "deck-a/bar/speaker"),
            ((920.0, None), "deck-a/bar/stream/pop"),
            ((1120.0, None), "deck-a/bar/cpu-label"),
            ((790.0, None), "deck-a/bar/clock/pop"),
            ((670.0, None), "deck-a/bar/rec"),
            ((590.0, None), "deck-a/bar/settings"),
            ((0.0, None), "deck-a/bar/window"),
        ],
    );
}

/// The drag strip and the wave share one place in the bar: the strip ends at
/// the width the wave starts at, so exactly one of them stands at any width.
#[kithara::test]
fn the_micro_drag_strip_hands_its_place_to_the_wave() {
    let ui = micro_preset();
    let cells = bar_cells(&ui, micro_bar(&ui));
    let band = |id: &str| {
        cells
            .iter()
            .find_map(|(band, cell)| (*cell == id).then_some(*band))
            .expect("the bar names the cell")
    };

    let (strip_from, strip_until) = band("deck-a/bar/drag");
    let (wave_from, wave_until) = band("deck-a/bar/wave");

    assert_eq!(strip_from, 0.0, "the strip stands at the narrowest bar");
    assert_eq!(
        strip_until,
        Some(wave_from),
        "the strip ends where the wave starts"
    );
    assert_eq!(wave_until, None, "the wave stands from its width up");
}

/// The micro preset grows from one bar into the player: each block appears
/// once the window is tall enough to hold it under the ones above it.
#[kithara::test]
fn the_micro_preset_takes_its_blocks_as_the_window_grows_taller() {
    let ui = micro_preset();

    assert_eq!(
        micro_blocks(&ui),
        vec![
            ((0.0, None), "deck-a/bar/menu/pop"),
            ((234.0, None), "deck-a/deck/wave"),
            ((494.0, None), "deck-a/library/tracks"),
        ],
    );
}

#[kithara::test]
fn player_preset_compiles_against_player_registry() {
    compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
}

#[kithara::test]
fn player_deck_starts_with_one_hero_wave() {
    let ui = compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Split { children, .. } = &ui.root else {
        panic!("expected split root");
    };
    let CompiledNode::Module { root, .. } = &children[1].node else {
        panic!("expected deck module");
    };
    let ExpandedNode::Column { children, .. } = &**root else {
        panic!("deck must compile to one column");
    };

    assert_eq!(children.len(), 3);
    let Some(ExpandedNode::Control {
        spec:
            ControlSpec::Wave {
                style: WaveStyle::Hero,
                zoom:
                    Some(Binding {
                        kind: BindingKind::Model,
                        id,
                        ..
                    }),
                ..
            },
        ..
    }) = children.first()
    else {
        panic!("expected hero wave with model-owned zoom");
    };
    assert_eq!(ui.resolve(*id), "deck.view.zoom");
}

#[kithara::test]
fn player_deck_compiles_canonical_transport_row() {
    let ui = compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Split { children, .. } = &ui.root else {
        panic!("expected split root");
    };
    let CompiledNode::Module { root, .. } = &children[1].node else {
        panic!("expected deck module");
    };
    let ExpandedNode::Column { children, .. } = &**root else {
        panic!("deck must compile to one column");
    };
    let Some(ExpandedNode::Row {
        size: Some(size),
        children,
        ..
    }) = children.get(1)
    else {
        panic!("expected transport row");
    };

    assert_eq!(size.h, Dim::Fixed(30.0));
    assert_eq!(children.len(), 12);
    for (index, icon) in [(8, IconName::ZoomOut), (9, IconName::ZoomIn)] {
        let Some(ExpandedNode::Control {
            spec:
                ControlSpec::Button {
                    icon: Some(declared),
                    frame: Some(frame),
                    ..
                },
            ..
        }) = children.get(index)
        else {
            panic!("expected a framed zoom cell at {index}");
        };
        assert_eq!(*declared, icon);
        assert!(frame.left, "{icon:?} must carry its left seam");
        assert!(!frame.top && !frame.right && !frame.bottom);
    }
    for index in 0..=6 {
        let Some(ExpandedNode::Control {
            spec: ControlSpec::Button { frame, .. },
            ..
        }) = children.get(index)
        else {
            panic!("expected a transport cell at {index}");
        };
        assert!(frame.is_none(), "cell {index} keeps the skin's own seam");
    }
    let Some(ExpandedNode::Optional { block, child }) = children.get(10) else {
        panic!("expected the stream block at 10");
    };
    assert_eq!(
        ui.resolve(block.hidden.key),
        "deck.stream.quality_hidden@deck=a"
    );
    let ExpandedNode::Popover { anchor, .. } = &**child else {
        panic!("the stream block holds the quality menu");
    };
    let ExpandedNode::Pressable { child: cell, .. } = &**anchor else {
        panic!("the menu opens from a pressable cell");
    };
    let ExpandedNode::Row {
        frame: Some(frame), ..
    } = &**cell
    else {
        panic!("expected a framed stream cell");
    };
    assert!(frame.left, "stream must carry its left seam");
    assert!(!frame.top && !frame.right && !frame.bottom);

    let Some(ExpandedNode::Row {
        id: Some(id),
        frame: Some(frame),
        ..
    }) = children.get(11)
    else {
        panic!("expected a framed cell at 11");
    };
    assert_eq!(ui.resolve(*id), "tempo");
    assert!(frame.left, "tempo must carry its left seam");
    assert!(!frame.top && !frame.right && !frame.bottom);
}

#[kithara::test]
fn player_preset_size_sums_global_deck_and_library_heights() {
    let ui = compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Split {
        axis,
        children,
        composed,
        ..
    } = &ui.root
    else {
        panic!("expected split root");
    };
    assert_eq!(*axis, Axis::Vertical);
    let CompiledNode::Module {
        size: global_size, ..
    } = &children[0].node
    else {
        panic!("expected global bar module");
    };
    let CompiledNode::Module {
        size: deck_size, ..
    } = &children[1].node
    else {
        panic!("expected deck module");
    };
    let CompiledNode::Module {
        size: library_size, ..
    } = &children[2].node
    else {
        panic!("expected library module");
    };

    assert_eq!(global_size.h.min(), 42.0);
    assert_eq!(deck_size.h.min(), 150.0);
    assert_eq!(library_size.h.min(), 210.0);
    assert_eq!(
        ui.size.h.min(),
        global_size.h.min() + deck_size.h.min() + library_size.h.min()
    );
    assert_eq!(ui.size, *composed);
}

/// The micro preset is a whole window: its bar stands the menu at every width,
/// so the menu is part of the builtin preset surface.
#[kithara::test]
fn the_app_menu_is_part_of_the_builtin_preset_surface() {
    builtin::resolver()
        .load(None, "modules/app-menu.kmodule.ron")
        .expect("the micro bar includes the app menu");
}
