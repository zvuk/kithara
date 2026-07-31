mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    error::UiDocError,
    expand::{Binding, BindingKind, ControlSpec, ExpandedNode},
    layout::Axis,
    module::{IconName, WaveStyle},
    size::Dim,
    source::{SourceResolver, UiConfig},
};

#[kithara::test]
fn micro_preset_compiles_against_player_registry() {
    let ui = compile(
        builtin::MICRO_PRESET,
        &builtin::resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected micro module");
    };
    let ExpandedNode::Row { children, .. } = &**root else {
        panic!("micro must compile to one row");
    };
    let specs: Vec<_> = children
        .iter()
        .filter_map(|child| match child {
            ExpandedNode::Control { spec, .. } => Some(spec),
            _ => None,
        })
        .collect();
    assert!(matches!(specs[0], ControlSpec::Button { .. }));
    assert!(matches!(specs[1], ControlSpec::DeckSummary { .. }));
    assert!(matches!(specs[2], ControlSpec::Bpm { .. }));
    assert!(matches!(specs[3], ControlSpec::VuStereo));
    assert!(matches!(specs[4], ControlSpec::Wave { .. }));
    assert!(matches!(specs[5], ControlSpec::SettingsButton));
}

#[kithara::test]
fn player_preset_compiles_against_player_registry() {
    compile(
        builtin::PLAYER_PRESET,
        &builtin::resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
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
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Split { children, .. } = &ui.root else {
        panic!("expected split root");
    };
    let CompiledNode::Module { root, .. } = &children[1].1 else {
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
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Split { children, .. } = &ui.root else {
        panic!("expected split root");
    };
    let CompiledNode::Module { root, .. } = &children[1].1 else {
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
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Split {
        axis,
        children,
        size,
        ..
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

    assert_eq!(global_size.h.min(), 42.0);
    assert_eq!(deck_size.h.min(), 150.0);
    assert_eq!(library_size.h.min(), 210.0);
    assert_eq!(
        ui.size.h.min(),
        global_size.h.min() + deck_size.h.min() + library_size.h.min()
    );
    assert_eq!(ui.size, *size);
}

#[kithara::test]
fn the_app_menu_is_a_shipped_asset_outside_builtin_preset_surface() {
    let error = builtin::resolver()
        .load(None, "modules/app-menu.kmodule.ron")
        .unwrap_err();

    assert!(
        matches!(error, UiDocError::NotFound { ref rel, .. } if rel == "modules/app-menu.kmodule.ron"),
        "{error}"
    );
}
