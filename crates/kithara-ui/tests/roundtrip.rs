use kithara_test_utils::kithara;
use kithara_ui::{
    error::UiDocError,
    ids::SourceUri,
    layout::{FrameSides, LayoutNode, parse_layout},
    module::{
        ChipStyle, ControlNode, GlyphStyle, IconName, PopoverAt, Priority, TextStyle, Tone,
        parse_module,
    },
    param::Param,
    size::{Dim, SizeSpec},
    skin::ColorRole,
};
use ron::extensions::Extensions;

fn origin() -> SourceUri {
    SourceUri("test.klayout.ron".into())
}

const TWO_MODULE_SPLIT: &str = r#"(
    schema: "kithara.layout",
    version: 1,
    id: "two",
    root: Split(
        axis: Horizontal,
        children: [
            (weight: 3.0, node: Module(instance: "deck-a", source: "modules/deck.kmodule.ron", with: { "deck": "a" })),
            (node: Module(instance: "library", source: "modules/library.kmodule.ron")),
        ],
    ),
)"#;

const DECK_MODULE: &str = r#"(
    schema: "kithara.module",
    version: 1,
    id: "deck",
    parameters: ["deck"],
    root: Column(
        children: [
            Text(
                id: "title",
                style: TrackTitle,
                read: Telemetry(id: "deck.track.title", with: { "deck": "$deck" }),
                adaptive: (priority: Required),
            ),
            Include(id: "transport", source: "deck/transport.kmodule.ron", with: { "deck": "$deck" }),
            Slot(id: "extra-controls"),
        ],
    ),
)"#;

const ROUNDTRIP_MODULE: &str = r#"(
    schema: "kithara.module",
    version: 1,
    id: "roundtrip",
    parameters: ["deck"],
    root: Column(
        id: "root",
        children: [
            Row(
                children: [
                    Button(
                        id: "play",
                        label: "PLAY",
                        active_label: Some("PAUSE"),
                        style: TransportPrimary,
                        read: Telemetry(id: "deck.playback.playing", with: { "deck": "$deck" }),
                        write: Command(id: "deck.transport.toggle_play", with: { "deck": "$deck" }),
                        size: Some((w: Fixed(96.0), h: Fixed(32.0))),
                        adaptive: (priority: Required),
                    ),
                    Scalar(
                        id: "load",
                        read: Telemetry(id: "deck.playback.position_normalized", with: { "deck": "$deck" }),
                        size: Some((w: Shrink, h: Fill)),
                        format: Percent,
                        framed: false,
                    ),
                    Fader(
                        id: "volume",
                        style: Volume,
                        read: Parameter(id: "player.output.volume"),
                        write: Parameter(id: "player.output.volume"),
                        size: Some((w: Fixed(120.0), h: Fill)),
                        adaptive: (priority: High),
                    ),
                    TrackList(
                        id: "tracks",
                        read: Model(id: "library.visible_tracks"),
                        columns: [Index, Deck, Title, Artist, Bpm, Key, Time, Energy, Transition],
                        columns_state: Some(Model(id: "ui.tracklist.columns")),
                        size: Some((w: Fill, h: Fixed(160.0))),
                        adaptive: (priority: Low),
                    ),
                ],
            ),
            Include(
                id: "transport",
                source: "deck/transport.kmodule.ron",
                with: { "deck": "$deck" },
            ),
            Slot(
                id: "extra",
                default: [
                    Column(
                        id: "nested",
                        children: [
                            Text(id: "status", adaptive: (priority: Normal)),
                        ],
                    ),
                ],
            ),
        ],
    ),
)"#;

fn to_ron_pretty<T: serde::Serialize>(value: &T) -> String {
    ron::Options::default()
        .with_default_extension(Extensions::IMPLICIT_SOME)
        .to_string_pretty(value, ron::ser::PrettyConfig::new())
        .expect("RON test serialization should succeed")
}

#[kithara::test]
fn layout_roundtrip_is_semantically_stable() {
    let doc = parse_layout(TWO_MODULE_SPLIT, &origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_layout(&printed, &origin()).unwrap();
    assert_eq!(doc, reparsed);
    assert_eq!(printed, to_ron_pretty(&reparsed));
}

#[kithara::test]
fn default_weight_is_one() {
    let doc = parse_layout(TWO_MODULE_SPLIT, &origin()).unwrap();
    let LayoutNode::Split { children, .. } = &doc.root else {
        panic!("expected split root");
    };
    assert_eq!(children[1].weight, 1.0);
}

#[kithara::test]
fn module_frame_sides_default_on_and_corner_ticks_default_off() {
    let defaults = parse_layout(
        r#"(schema: "kithara.layout", version: 1, id: "defaults",
            root: Module(instance: "a", source: "m.ron"))"#,
        &origin(),
    )
    .unwrap();
    let LayoutNode::Module { frame, corners, .. } = &defaults.root else {
        panic!("expected module root");
    };
    assert_eq!(*frame, FrameSides::default());
    assert!(!corners);

    let configured = parse_layout(
        r#"(schema: "kithara.layout", version: 1, id: "configured",
            root: Module(
                instance: "a",
                source: "m.ron",
                frame: (right: false),
                corners: true,
            ))"#,
        &origin(),
    )
    .unwrap();
    let LayoutNode::Module { frame, corners, .. } = &configured.root else {
        panic!("expected module root");
    };
    assert!(frame.top);
    assert!(!frame.right);
    assert!(frame.bottom);
    assert!(frame.left);
    assert!(corners);
}

#[kithara::test]
fn resize_edges_are_off_unless_the_layout_asks() {
    let plain = parse_layout(TWO_MODULE_SPLIT, &origin()).unwrap();
    assert!(!plain.resize_edges);

    let framed = parse_layout(
        r#"(schema: "kithara.layout", version: 1, id: "framed", resize_edges: true,
            root: Module(instance: "a", source: "m.ron"))"#,
        &origin(),
    )
    .unwrap();
    assert!(framed.resize_edges);
}

#[kithara::test]
fn every_size_rule_parses_including_content_measured_axes() {
    let doc = parse_module(
        r#"(schema: "kithara.module", version: 1, id: "sizes",
            root: Row(
                id: "row",
                size: (w: Shrink, h: Fill),
                children: [
                    Text(id: "label", size: (w: Shrink, h: Fixed(18.0))),
                    Text(id: "span", size: (w: Range(min: 20.0, max: Some(40.0)), h: Fill)),
                ],
            ))"#,
        &SourceUri("sizes.kmodule.ron".into()),
    )
    .unwrap();
    let ControlNode::Row { size, children, .. } = &doc.root else {
        panic!("expected row root");
    };

    assert_eq!(*size, Some(SizeSpec::new(Dim::Shrink, Dim::Fill)));
    let ControlNode::Text { size, .. } = &children[0] else {
        panic!("expected text child");
    };
    assert_eq!(*size, Some(SizeSpec::new(Dim::Shrink, Dim::Fixed(18.0))));
    let ControlNode::Text { size, .. } = &children[1] else {
        panic!("expected text child");
    };
    assert_eq!(
        *size,
        Some(SizeSpec::new(
            Dim::Range {
                min: 20.0,
                max: Some(40.0)
            },
            Dim::Fill
        ))
    );
}

#[kithara::test]
fn unknown_field_is_rejected() {
    let text = r#"(schema: "kithara.layout", version: 1, id: "x",
        root: Module(instance: "a", source: "m.ron", extra: 1.0))"#;
    let error = parse_layout(text, &origin()).unwrap_err();
    assert!(matches!(error, UiDocError::Syntax { .. }));
}

#[kithara::test]
fn module_document_is_rejected_as_layout() {
    let text = r#"(schema: "kithara.module", version: 1, id: "m", root: ())"#;
    let error = parse_layout(text, &origin()).unwrap_err();
    assert!(matches!(
        error,
        UiDocError::WrongDocKind {
            expected: "layout",
            ..
        }
    ));
}

fn module_origin() -> SourceUri {
    SourceUri("deck.kmodule.ron".into())
}

#[kithara::test]
fn module_parses_with_implicit_some_bindings() {
    let doc = parse_module(DECK_MODULE, &module_origin()).unwrap();
    assert_eq!(doc.parameters, vec!["deck".to_owned()]);
    let ControlNode::Column { children, .. } = &doc.root else {
        panic!("expected column root");
    };
    assert_eq!(children.len(), 3);
    let ControlNode::Text { read, adaptive, .. } = &children[0] else {
        panic!("expected control");
    };
    assert!(read.is_some());
    assert_eq!(adaptive.priority, Priority::Required);
}

#[kithara::test]
fn module_roundtrip_is_semantically_stable() {
    let doc = parse_module(ROUNDTRIP_MODULE, &module_origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_module(&printed, &module_origin()).unwrap();
    assert_eq!(doc, reparsed);
    assert_eq!(printed, to_ron_pretty(&reparsed));
}

#[kithara::test]
fn include_arguments_are_preserved() {
    let doc = parse_module(DECK_MODULE, &module_origin()).unwrap();
    let ControlNode::Column { children, .. } = &doc.root else {
        panic!("expected column root");
    };
    let ControlNode::Include { with, .. } = &children[1] else {
        panic!("expected include");
    };
    assert_eq!(with.get("deck").map(String::as_str), Some("$deck"));
}

#[kithara::test]
fn navigation_controls_roundtrip_with_typed_icons() {
    let text = r#"(
        schema: "kithara.module",
        version: 1,
        id: "navigation",
        root: Column(children: [
            Glyph(id: "header-icon", icon: Gear),
            NavItem(
                id: "modules",
                label: "MODULES",
                icon: Playlist,
                read: Model(id: "gallery.tab.modules"),
                write: Command(id: "gallery.tab.modules"),
            ),
            TabLarge(
                id: "deck",
                label: "DECK",
                read: Model(id: "gallery.module.deck"),
                write: Command(id: "gallery.module.deck"),
            ),
        ]),
    )"#;

    let doc = parse_module(text, &module_origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_module(&printed, &module_origin()).unwrap();
    assert_eq!(doc, reparsed);

    let ControlNode::Column { children, .. } = &doc.root else {
        panic!("expected column root");
    };
    assert!(matches!(
        &children[0],
        ControlNode::Glyph {
            icon: Param::Fixed(IconName::Gear),
            ..
        }
    ));
    assert!(matches!(
        &children[1],
        ControlNode::NavItem {
            icon: Param::Fixed(IconName::Playlist),
            ..
        }
    ));
    assert!(matches!(&children[2], ControlNode::TabLarge { .. }));
}

#[kithara::test]
fn design_system_controls_and_text_roles_roundtrip() {
    let text = r#"(
        schema: "kithara.module",
        version: 1,
        id: "design-system",
        root: Column(children: [
            Text(id: "brand", style: Brand, label: "KITHARA"),
            Text(id: "deck", style: DeckLetter, label: "A B C D"),
            Text(id: "telemetry", style: Telemetry, label: "124.00"),
            Text(id: "micro", style: MicroLabel, label: "MICRO LABEL"),
            Segmented(id: "beat", items: ["MICRO", "1", "2", "4"]),
            Select(id: "device", label: "Scarlett 4i4 USB"),
            StatusDot(id: "ok", label: "OK", tone: Success),
            Cell(id: "size", label: Some("26"), highlighted: true),
        ]),
    )"#;

    let doc = parse_module(text, &module_origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_module(&printed, &module_origin()).unwrap();
    assert_eq!(doc, reparsed);

    let ControlNode::Column { children, .. } = &doc.root else {
        panic!("expected column root");
    };
    assert!(matches!(
        children[0],
        ControlNode::Text {
            style: TextStyle::Brand,
            ..
        }
    ));
    assert!(matches!(
        children[4],
        ControlNode::Segmented { ref items, .. } if items.len() == 4
    ));
    assert!(matches!(
        children[6],
        ControlNode::StatusDot {
            tone: Tone::Success,
            ..
        }
    ));
    assert!(matches!(
        children[7],
        ControlNode::Cell {
            highlighted: true,
            ..
        }
    ));
}

#[kithara::test]
fn chip_style_is_typed_and_defaults_to_deck() {
    let text = r#"(
        schema: "kithara.module",
        version: 1,
        id: "chips",
        root: Row(children: [
            Chip(id: "deck", label: "A"),
            Chip(id: "routing", label: "FX1", style: Routing),
        ]),
    )"#;

    let doc = parse_module(text, &module_origin()).unwrap();
    let ControlNode::Row { children, .. } = &doc.root else {
        panic!("expected row root");
    };
    assert!(matches!(
        &children[0],
        ControlNode::Chip {
            style: ChipStyle::Deck,
            ..
        }
    ));
    assert!(matches!(
        &children[1],
        ControlNode::Chip {
            style: ChipStyle::Routing,
            ..
        }
    ));
}

const OPTIONAL_LAYOUT: &str = r#"(
    schema: "kithara.layout",
    version: 1,
    id: "optional",
    root: Split(
        axis: Horizontal,
        children: [
            (node: Optional(
                id: "library",
                hidden: Model(id: "ui.block.hidden"),
                node: Module(instance: "library", source: "modules/library.kmodule.ron"),
            )),
            (node: Module(instance: "deck-a", source: "modules/deck.kmodule.ron", with: { "deck": "a" })),
        ],
    ),
)"#;

const OPTIONAL_MODULE: &str = r#"(
    schema: "kithara.module",
    version: 1,
    id: "optional",
    root: Column(
        children: [
            Optional(
                id: "eq",
                hidden: Model(id: "ui.block.hidden"),
                child: Row(
                    children: [
                        Knob(id: "low", label: Some("LOW")),
                        Knob(id: "high", label: Some("HIGH")),
                    ],
                ),
            ),
            Knob(id: "volume"),
        ],
    ),
)"#;

#[kithara::test]
fn an_optional_layout_block_roundtrips() {
    let doc = parse_layout(OPTIONAL_LAYOUT, &origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_layout(&printed, &origin()).unwrap();

    assert_eq!(doc, reparsed);
    assert_eq!(printed, to_ron_pretty(&reparsed));
}

#[kithara::test]
fn an_optional_module_block_roundtrips() {
    let doc = parse_module(OPTIONAL_MODULE, &module_origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_module(&printed, &module_origin()).unwrap();

    assert_eq!(doc, reparsed);
    assert_eq!(printed, to_ron_pretty(&reparsed));
}

#[kithara::test]
fn an_optional_block_wraps_exactly_one_child() {
    let doc = parse_module(OPTIONAL_MODULE, &module_origin()).unwrap();
    let ControlNode::Column { children, .. } = &doc.root else {
        panic!("expected column root");
    };
    let ControlNode::Optional { id, child, .. } = &children[0] else {
        panic!("expected an optional block");
    };

    assert_eq!(id.0, "eq");
    assert!(matches!(**child, ControlNode::Row { .. }));
}

const POPOVER_MODULE: &str = r#"(
    schema: "kithara.module",
    version: 1,
    id: "app-menu",
    root: Row(
        children: [
            Popover(
                id: "menu",
                open: Model(id: "ui.menu.open"),
                anchor: Pressable(
                    id: "burger",
                    press: Command(id: "ui.menu.toggle"),
                    child: Row(
                        id: "burger-cell",
                        size: (w: Fixed(36.0), h: Fill),
                        background: BgSelect,
                        active: Model(id: "ui.menu.open"),
                        active_background: BgPanel,
                        frame_color: LineInner,
                        active_frame_color: Accent,
                        children: [
                            Glyph(id: "burger-icon", icon: Menu, style: MenuBurger, color: TextDim),
                        ],
                    ),
                ),
                content: Column(
                    id: "pop",
                    size: (w: Fixed(298.0), h: Shrink),
                    children: [
                        Row(
                            id: "header",
                            children: [
                                Text(id: "brand", style: BrandSmall, label: "KITHARA"),
                                Text(id: "version", style: MicroLabel, label: "2.4.1"),
                            ],
                        ),
                        Pressable(
                            id: "modules",
                            press: Command(id: "ui.menu.toggle"),
                            child: Row(
                                id: "modules-row",
                                children: [
                                    Glyph(
                                        id: "modules-caret",
                                        icon: ChevronRight,
                                        active_icon: ChevronDown,
                                        style: Menu,
                                        color: Muted,
                                        active_color: Accent,
                                        active: Model(id: "ui.menu.open"),
                                    ),
                                    Text(
                                        id: "modules-label",
                                        style: Mono,
                                        color: Muted,
                                        active_color: Text,
                                        label: "MODULES",
                                        active: Model(id: "ui.menu.open"),
                                    ),
                                ],
                            ),
                        ),
                    ],
                ),
            ),
        ],
    ),
)"#;

#[kithara::test]
fn a_popover_with_a_pressable_anchor_roundtrips() {
    let doc = parse_module(POPOVER_MODULE, &module_origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_module(&printed, &module_origin()).unwrap();

    assert_eq!(doc, reparsed);
    assert_eq!(printed, to_ron_pretty(&reparsed));
}

const POINTER_POPOVER_MODULE: &str = r#"(
    schema: "kithara.module",
    version: 1,
    id: "context-menu",
    root: Column(
        children: [
            Popover(
                id: "row-1-menu",
                open: Model(id: "library.menu.open", with: { "row": "1" }),
                at: Pointer,
                anchor: Pressable(
                    id: "row-1",
                    press: Command(id: "library.menu.select", with: { "row": "1" }),
                    child: Row(id: "row-1-cell", size: (w: Fill, h: Fixed(30.0)), children: []),
                ),
                content: Column(
                    id: "pop",
                    size: (w: Fixed(180.0), h: Shrink),
                    children: [
                        Text(id: "play", style: Mono, color: Text, label: "PLAY"),
                    ],
                ),
            ),
        ],
    ),
)"#;

#[kithara::test]
fn a_popover_opening_at_the_pointer_roundtrips() {
    let doc = parse_module(POINTER_POPOVER_MODULE, &module_origin()).unwrap();
    let printed = to_ron_pretty(&doc);
    let reparsed = parse_module(&printed, &module_origin()).unwrap();

    assert_eq!(doc, reparsed);
    assert_eq!(printed, to_ron_pretty(&reparsed));

    let ControlNode::Column { children, .. } = &doc.root else {
        panic!("expected column root");
    };
    let ControlNode::Popover { at, .. } = &children[0] else {
        panic!("expected a popover");
    };

    assert_eq!(*at, PopoverAt::Pointer);
}

#[kithara::test]
fn a_popover_carries_an_anchor_and_an_arbitrary_content_subtree() {
    let doc = parse_module(POPOVER_MODULE, &module_origin()).unwrap();
    let ControlNode::Row { children, .. } = &doc.root else {
        panic!("expected row root");
    };
    let ControlNode::Popover {
        id,
        at,
        anchor,
        content,
        ..
    } = &children[0]
    else {
        panic!("expected a popover");
    };

    assert_eq!(id.0, "menu");
    assert_eq!(
        *at,
        PopoverAt::Anchor,
        "a popover that declares no geometry opens from its anchor"
    );
    let ControlNode::Pressable { id, child, .. } = &**anchor else {
        panic!("expected a pressable anchor");
    };
    assert_eq!(id.0, "burger");
    assert!(matches!(**child, ControlNode::Row { .. }));
    assert!(matches!(**content, ControlNode::Column { .. }));
}

#[kithara::test]
fn a_row_carries_its_active_tone_and_frame_roles() {
    let doc = parse_module(POPOVER_MODULE, &module_origin()).unwrap();
    let ControlNode::Row { children, .. } = &doc.root else {
        panic!("expected row root");
    };
    let ControlNode::Popover { anchor, .. } = &children[0] else {
        panic!("expected a popover");
    };
    let ControlNode::Pressable { child, .. } = &**anchor else {
        panic!("expected a pressable anchor");
    };
    let ControlNode::Row {
        active,
        background,
        active_background,
        frame_color,
        active_frame_color,
        ..
    } = &**child
    else {
        panic!("expected the burger cell");
    };

    assert!(active.is_some());
    assert_eq!(*background, Some(ColorRole::BgSelect));
    assert_eq!(*active_background, Some(ColorRole::BgPanel));
    assert_eq!(*frame_color, Some(ColorRole::LineInner));
    assert_eq!(*active_frame_color, Some(ColorRole::Accent));
}

#[kithara::test]
fn a_glyph_and_a_text_carry_the_tones_flags_and_icons_they_name() {
    let doc = parse_module(POPOVER_MODULE, &module_origin()).unwrap();
    let ControlNode::Row { children, .. } = &doc.root else {
        panic!("expected row root");
    };
    let ControlNode::Popover { content, .. } = &children[0] else {
        panic!("expected a popover");
    };
    let ControlNode::Column { children, .. } = &**content else {
        panic!("expected the pop column");
    };
    let ControlNode::Pressable { child, .. } = &children[1] else {
        panic!("expected the modules row");
    };
    let ControlNode::Row { children, .. } = &**child else {
        panic!("expected a row");
    };
    let ControlNode::Glyph {
        icon,
        active_icon,
        style,
        color,
        active_color,
        active,
        ..
    } = &children[0]
    else {
        panic!("expected the caret glyph");
    };

    assert_eq!(*icon, Param::Fixed(IconName::ChevronRight));
    assert_eq!(*active_icon, Some(Param::Fixed(IconName::ChevronDown)));
    assert_eq!(*style, GlyphStyle::Menu);
    assert_eq!(*color, Some(ColorRole::Muted));
    assert_eq!(*active_color, Some(ColorRole::Accent));
    assert!(active.is_some());

    let ControlNode::Text {
        style,
        color,
        active_color,
        active,
        ..
    } = &children[1]
    else {
        panic!("expected the modules label");
    };

    assert_eq!(*style, TextStyle::Mono);
    assert_eq!(*color, Some(ColorRole::Muted));
    assert_eq!(*active_color, Some(ColorRole::Text));
    assert!(active.is_some());
}
