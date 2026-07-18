use kithara_test_utils::kithara;
use kithara_ui::{
    error::UiDocError,
    ids::SourceUri,
    layout::{LayoutNode, parse_layout},
    module::{ControlNode, Priority, parse_module},
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
            Control(
                id: "title",
                kind: "text",
                props: { "style": Text("track-title") },
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
                    Control(
                        id: "play",
                        kind: "button",
                        props: {
                            "enabled": Bool(true),
                            "gain": Num(0.5),
                            "label": Text("PLAY"),
                        },
                        read: Telemetry(id: "deck.playback.playing", with: { "deck": "$deck" }),
                        write: Command(id: "deck.transport.toggle_play", with: { "deck": "$deck" }),
                        adaptive: (priority: Required, min_w: 96.0, min_h: 32.0),
                    ),
                    Control(
                        id: "volume",
                        kind: "fader.horizontal",
                        read: Parameter(id: "player.output.volume"),
                        write: Parameter(id: "player.output.volume"),
                        adaptive: (priority: High, min_w: 120.0),
                    ),
                    Control(
                        id: "tracks",
                        kind: "track_list",
                        read: Model(id: "library.visible_tracks"),
                        adaptive: (priority: Low, min_h: 160.0),
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
                            Control(id: "status", kind: "text", adaptive: (priority: Normal)),
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
    let ControlNode::Control { read, adaptive, .. } = &children[0] else {
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
