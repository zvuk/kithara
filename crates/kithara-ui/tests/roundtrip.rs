use kithara_test_utils::kithara;
use kithara_ui::{
    error::UiDocError,
    ids::SourceUri,
    layout::{LayoutNode, parse_layout},
    module::{ChipStyle, ControlNode, IconName, Priority, parse_module},
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
            icon: IconName::Gear,
            ..
        }
    ));
    assert!(matches!(
        &children[1],
        ControlNode::NavItem {
            icon: IconName::Playlist,
            ..
        }
    ));
    assert!(matches!(&children[2], ControlNode::TabLarge { .. }));
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
