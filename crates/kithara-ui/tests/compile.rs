mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi, compile},
    error::UiDocError,
    expand::{Binding, BindingKind, ControlSpec, ExpandedNode},
    module::{ChromeStyle, IconName, PopoverAt, TrackColumn},
    registry::{EndpointCategory, EndpointDesc, ValueKind},
    size::{Dim, SizeSpec},
    source::{Limits, MemResolver, UiConfig},
};

fn resolver() -> MemResolver {
    builtin::resolver()
}

fn track_list_resolver(module: &str) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "track-list.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "track-list",
            root: Module(instance: "track-list", source: "track-list.kmodule.ron"))"#,
    );
    resolver.insert("track-list.kmodule.ron", module);
    resolver
}

#[kithara::test]
fn compiles_micro_layout_end_to_end() {
    let ui = compile(
        "micro.klayout.ron",
        &resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { instance, .. } = &ui.root else {
        panic!("expected module root");
    };
    assert_eq!(ui.resolve(*instance), "deck-a");
}

#[kithara::test]
fn crossfader_compiles_with_scalar_read_and_write_bindings() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "mixer.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "mixer",
            root: Module(instance: "mixer", source: "mixer.kmodule.ron"))"#,
    );
    resolver.insert(
        "mixer.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Crossfader(
                id: "xfade",
                read: Parameter(id: "mixer.xfade"),
                write: Parameter(id: "mixer.xfade"),
            ))"#,
    );
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Parameter,
        "mixer.xfade",
        EndpointDesc::new(ValueKind::Scalar),
    );

    let ui = compile(
        "mixer.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Control {
        spec: ControlSpec::Crossfader { ticks: false },
        read: Some(Binding {
            kind: BindingKind::Parameter,
            ..
        }),
        write: Some(Binding {
            kind: BindingKind::Parameter,
            ..
        }),
        ..
    } = &**root
    else {
        panic!("expected compiled crossfader");
    };
}

#[kithara::test]
fn meter_reads_a_scalar_and_refuses_any_other_kind() {
    let module = |endpoint| {
        format!(
            r#"(schema: "kithara.module", version: 1, id: "bar",
                root: Meter(id: "load", read: Telemetry(id: "{endpoint}")))"#
        )
    };
    let mut resolver = MemResolver::default();
    resolver.insert(
        "bar.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "bar",
            root: Module(instance: "bar", source: "bar.kmodule.ron"))"#,
    );
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Telemetry,
        "engine.load",
        EndpointDesc::new(ValueKind::Scalar),
    );

    resolver.insert("bar.kmodule.ron", &module("engine.load"));
    let ui = compile(
        "bar.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    assert!(matches!(
        &**root,
        ExpandedNode::Control {
            spec: ControlSpec::Meter,
            read: Some(Binding {
                kind: BindingKind::Telemetry,
                ..
            }),
            ..
        }
    ));

    resolver.insert("bar.kmodule.ron", &module("deck.playback.playing"));
    let error = compile(
        "bar.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();

    assert!(matches!(error, UiDocError::BindingType { .. }));
}

#[kithara::test]
fn vis_compiles_with_scalar_read_and_select_index_write() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "vis.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "vis",
            root: Module(instance: "vis", source: "vis.kmodule.ron"))"#,
    );
    resolver.insert(
        "vis.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "vis",
            root: Vis(
                id: "shader",
                read: Model(id: "vis.preset"),
                write: Parameter(id: "vis.preset"),
            ))"#,
    );
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Model,
        "vis.preset",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "vis.preset",
        EndpointDesc::new(ValueKind::Scalar),
    );

    let ui = compile(
        "vis.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Control {
        spec: ControlSpec::Vis,
        read: Some(Binding {
            kind: BindingKind::Model,
            ..
        }),
        write: Some(Binding {
            kind: BindingKind::Parameter,
            ..
        }),
        ..
    } = &**root
    else {
        panic!("expected compiled VIS control");
    };
}

#[kithara::test]
fn vis_rejects_non_scalar_read_and_write_bindings() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "vis.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "vis",
            root: Module(instance: "vis", source: "vis.kmodule.ron"))"#,
    );
    resolver.insert(
        "vis.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "vis",
            root: Vis(
                id: "shader",
                read: Model(id: "vis.preset"),
                write: Parameter(id: "vis.preset"),
            ))"#,
    );
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Model,
        "vis.preset",
        EndpointDesc::new(ValueKind::Text),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "vis.preset",
        EndpointDesc::new(ValueKind::Bool),
    );

    let read_error = compile(
        "vis.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();
    assert!(matches!(
        read_error,
        UiDocError::BindingType { id, expected, got, .. }
            if id == "vis.preset" && expected == "Scalar" && got == "Text"
    ));

    registry.insert(
        EndpointCategory::Model,
        "vis.preset",
        EndpointDesc::new(ValueKind::Scalar),
    );
    let write_error = compile(
        "vis.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();
    assert!(matches!(
        write_error,
        UiDocError::BindingType { id, expected, got, .. }
            if id == "vis.preset" && expected == "Scalar" && got == "Bool"
    ));
}

#[kithara::test]
fn track_list_requires_title_column_at_compile_time() {
    let resolver = track_list_resolver(
        r#"(schema: "kithara.module", version: 1, id: "track-list",
            root: TrackList(
                id: "tracks",
                columns: [Index, Artist],
                read: Model(id: "library.visible_tracks"),
            ))"#,
    );

    let error = compile(
        "track-list.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        UiDocError::MissingTrackTitleColumn { path, .. } if path == "track-list/tracks"
    ));
}

#[kithara::test]
fn track_list_compiles_typed_columns_and_optional_state_prefix() {
    let resolver = track_list_resolver(
        r#"(schema: "kithara.module", version: 1, id: "track-list",
            root: TrackList(
                id: "tracks",
                columns: [Index, Title, Bpm],
                columns_state: Some(Model(id: "ui.tracklist.columns")),
                read: Model(id: "library.visible_tracks"),
            ))"#,
    );
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.tracklist.columns.title",
        EndpointDesc::new(ValueKind::Bool),
    );

    let ui = compile(
        "track-list.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Control {
        spec:
            ControlSpec::TrackList {
                columns,
                columns_state,
                ..
            },
        ..
    } = &**root
    else {
        panic!("expected track list control");
    };

    assert_eq!(
        columns,
        &[TrackColumn::Index, TrackColumn::Title, TrackColumn::Bpm]
    );
    let Some(Binding {
        kind: BindingKind::Model,
        id,
        ..
    }) = columns_state
    else {
        panic!("expected model state prefix");
    };
    assert_eq!(ui.resolve(*id), "ui.tracklist.columns");
}

#[kithara::test]
fn present_track_list_column_state_endpoint_must_be_bool() {
    let resolver = track_list_resolver(
        r#"(schema: "kithara.module", version: 1, id: "track-list",
            root: TrackList(
                id: "tracks",
                columns: [Title],
                columns_state: Some(Model(id: "ui.tracklist.columns")),
                read: Model(id: "library.visible_tracks"),
            ))"#,
    );
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.tracklist.columns.title",
        EndpointDesc::new(ValueKind::Text),
    );

    let error = compile(
        "track-list.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        UiDocError::BindingType { id, expected, got, .. }
            if id == "ui.tracklist.columns.title" && expected == "Bool" && got == "Text"
    ));
}

#[kithara::test]
fn layout_module_size_override_wins_over_computed_size() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "override.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "override",
            root: Module(
                instance: "deck-a",
                source: "module.kmodule.ron",
                size: Some((w: Fixed(100.0), h: Fixed(50.0))),
            ))"#,
    );
    resolver.insert(
        "module.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "module",
            root: Button(id: "play", label: "PLAY"))"#,
    );

    let ui = compile(
        "override.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let expected = SizeSpec::new(Dim::Fixed(100.0), Dim::Fixed(50.0));
    let CompiledNode::Module { size, .. } = &ui.root else {
        panic!("expected module root");
    };

    assert_eq!(*size, expected);
    assert_eq!(ui.size, expected);
}

#[kithara::test]
fn module_shell_metadata_compiles_into_the_module_node() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "shell.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "shell",
            root: Module(
                instance: "deck-a",
                source: "shell.kmodule.ron",
                with: { "deck": "a" },
                frame: (top: true, right: false, bottom: true, left: false),
                corners: true,
            ))"#,
    );
    resolver.insert(
        "shell.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
            title: Some("Deck"), chip: Some("DECK"), assign: ["A", "B"], chrome: Full,
            footer: Some(Telemetry(id: "deck.track.title", with: { "deck": "$deck" })),
            root: Text(id: "label"))"#,
    );

    let ui = compile(
        "shell.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module {
        module,
        title,
        chip,
        assign,
        chrome,
        frame,
        corners,
        footer,
        collapsed,
        ..
    } = &ui.root
    else {
        panic!("expected module root");
    };

    assert_eq!(ui.resolve(*module), "deck");
    assert_eq!(title.map(|id| ui.resolve(id)), Some("Deck"));
    assert_eq!(chip.map(|id| ui.resolve(id)), Some("DECK"));
    assert_eq!(
        assign.iter().map(|id| ui.resolve(*id)).collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert_eq!(*chrome, ChromeStyle::Full);
    assert!(frame.top);
    assert!(!frame.right);
    assert!(frame.bottom);
    assert!(!frame.left);
    assert!(*corners);
    assert_eq!(ui.resolve(*collapsed), "ui.module.deck.collapsed");
    let Some(Binding {
        kind: BindingKind::Telemetry,
        id,
        key,
        with,
        ..
    }) = footer
    else {
        panic!("expected telemetry footer");
    };
    assert_eq!(ui.resolve(*id), "deck.track.title");
    assert_eq!(ui.resolve(*key), "deck.track.title@deck=a");
    assert_eq!(
        with.iter()
            .map(|(key, value)| (ui.resolve(*key), ui.resolve(*value)))
            .collect::<Vec<_>>(),
        vec![("deck", "a")]
    );
}

#[kithara::test]
fn module_footer_requires_a_text_read_endpoint() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "shell.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "shell",
            root: Module(instance: "deck-a", source: "shell.kmodule.ron"))"#,
    );
    resolver.insert(
        "shell.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck",
            footer: Some(Parameter(id: "player.output.volume")),
            root: Text(id: "label"))"#,
    );

    let error = compile(
        "shell.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();

    assert!(
        matches!(
            &error,
            UiDocError::BindingType { expected, got, .. }
                if expected == "Text" && got == "Scalar"
        ),
        "{error:?}"
    );
}

#[kithara::test]
fn unknown_endpoint_fails_with_module_origin_and_path() {
    let mut resolver = resolver();
    resolver.insert(
        "modules/deck/transport.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "transport", parameters: ["deck"],
            root: Button(id: "play", label: "PLAY",
                write: Command(id: "deck.transport.typo", with: { "deck": "$deck" })))"#,
    );
    let error = compile(
        "player.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        UiDocError::UnknownEndpoint { id, .. } if id == "deck.transport.typo"
    ));
    let message = error.to_string();
    assert!(
        message.contains("modules/deck/transport.kmodule.ron"),
        "{message}"
    );
    assert!(message.contains("deck-a/transport/play"), "{message}");
}

#[kithara::test]
fn node_limit_is_enforced() {
    let mut limits = Limits::default();
    limits.max_nodes = 1;
    let error = compile(
        "micro.klayout.ron",
        &resolver(),
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::builder().limits(limits).build(),
    )
    .unwrap_err();
    assert!(matches!(error, UiDocError::NodesExceeded { max: 1, .. }));
}

#[kithara::test]
fn layout_parameter_reference_is_unresolved() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "layout.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "layout",
            root: Module(instance: "deck-a", source: "deck.kmodule.ron", with: {
                "deck": "$deck",
            }))"#,
    );
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
            root: Text(id: "title"))"#,
    );

    let error = compile(
        "layout.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        UiDocError::UnresolvedParam { origin, name, path }
            if origin.0 == "layout.klayout.ron" && name == "deck" && path == "deck-a"
    ));
}

#[kithara::test]
fn layout_doubled_dollar_passes_literal_dollar() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "literal.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "literal",
            root: Module(instance: "literal", source: "literal.kmodule.ron", with: {
                "x": "$$lit",
            }))"#,
    );
    resolver.insert(
        "literal.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "literal", parameters: ["x"],
            root: Chip(id: "text", label: "$x"))"#,
    );

    let ui = compile(
        "literal.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module");
    };
    let ExpandedNode::Control {
        spec: ControlSpec::Chip { label, .. },
        ..
    } = &**root
    else {
        panic!("expected control");
    };
    assert_eq!(ui.resolve(*label), "$lit");
}

#[kithara::test]
fn oversized_layout_source_is_rejected() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "large.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "large",
            root: Module(instance: "deck-a", source: "deck.kmodule.ron"))"#,
    );
    let mut limits = Limits::default();
    limits.max_bytes = 32;

    let error = compile(
        "large.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::builder().limits(limits).build(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        UiDocError::TooLarge {
            origin,
            bytes,
            max: 32,
        } if origin.0 == "large.klayout.ron" && bytes > 32
    ));
}

#[kithara::test]
fn fifty_empty_columns_exceed_node_limit() {
    let children = vec!["Column(children: [])"; 50].join(",");
    let root = format!("Column(children: [{children}])");
    let mut resolver = MemResolver::default();
    resolver.insert(
        "nested.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "nested",
            root: Module(instance: "nested", source: "nested.kmodule.ron"))"#,
    );
    resolver.insert(
        "nested.kmodule.ron",
        &format!(r#"(schema: "kithara.module", version: 1, id: "nested", root: {root})"#),
    );
    let mut limits = Limits::default();
    limits.max_nodes = 10;

    let error = compile(
        "nested.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::builder().limits(limits).build(),
    )
    .unwrap_err();
    assert!(
        matches!(
            &error,
            UiDocError::NodesExceeded {
                count: 11,
                max: 10,
                ..
            }
        ),
        "{error:?}"
    );
}

#[kithara::test]
fn knob_caption_is_document_text_and_optional() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "knobs.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "knobs",
            root: Module(instance: "deck-a", source: "knobs.kmodule.ron"))"#,
    );
    resolver.insert(
        "knobs.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "knobs",
            root: Row(children: [
                Knob(id: "low", label: Some("LOW")),
                Knob(id: "mid"),
            ]))"#,
    );

    let ui = compile(
        "knobs.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module");
    };
    let ExpandedNode::Row { children, .. } = &**root else {
        panic!("expected row");
    };
    let captions: Vec<Option<&str>> = children
        .iter()
        .map(|child| {
            let ExpandedNode::Control {
                spec: ControlSpec::Knob { label },
                ..
            } = child
            else {
                panic!("expected knob");
            };
            label.map(|id| ui.resolve(id))
        })
        .collect();

    assert_eq!(captions, vec![Some("LOW"), None]);
}

fn block_registry() -> common::TestRegistry {
    let mut registry = common::player_registry();
    registry.insert(
        EndpointCategory::Model,
        "ui.block.hidden",
        EndpointDesc::new(ValueKind::Bool),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.block.deck_hidden",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.menu.open",
        EndpointDesc::new(ValueKind::Bool),
    );
    registry.insert(
        EndpointCategory::Command,
        "ui.menu.toggle",
        EndpointDesc::new(ValueKind::Trigger),
    );
    registry
}

fn block_resolver(module: &str) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "blocks.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "blocks",
            root: Module(instance: "mixer", source: "blocks.kmodule.ron"))"#,
    );
    resolver.insert("blocks.kmodule.ron", module);
    resolver
}

fn compile_blocks(resolver: &MemResolver, entry: &str) -> Result<CompiledUi, UiDocError> {
    compile(
        entry,
        resolver,
        &block_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
}

#[kithara::test]
fn a_duplicate_block_id_is_rejected() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                    child: Knob(id: "low")),
                Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                    child: Knob(id: "high")),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, .. } if id == "eq"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_block_id_collides_with_a_control_id_in_the_same_module() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "low", hidden: Model(id: "ui.block.hidden"),
                    child: Knob(id: "high")),
                Knob(id: "low"),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, .. } if id == "low"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_control_under_a_block_keeps_its_id_checked() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                    child: Row(children: [
                        Knob(id: "low"),
                        Knob(id: "low"),
                    ])),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, .. } if id == "low"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_block_id_carrying_an_address_separator_is_rejected() {
    for id in ["mixer.eq", "eq@deck=a"] {
        let resolver = block_resolver(&format!(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Column(children: [
                    Optional(id: "{id}", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "low")),
                ]))"#
        ));

        let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

        assert!(
            matches!(&error, UiDocError::InvalidId { id: invalid, .. } if invalid == id),
            "`{id}`: {error:?}"
        );
    }
}

#[kithara::test]
fn a_layout_block_id_may_not_collide_with_an_instance() {
    let mut resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Knob(id: "low"))"#,
    );
    resolver.insert(
        "collide.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "collide",
            root: Split(axis: Horizontal, children: [
                (node: Module(instance: "mixer", source: "blocks.kmodule.ron")),
                (node: Optional(id: "mixer", hidden: Model(id: "ui.block.hidden"),
                    node: Module(instance: "other", source: "blocks.kmodule.ron"))),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "collide.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::DuplicateId { id, .. } if id == "mixer"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_module_block_compiles_to_its_expanded_path_and_binding() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                    child: Knob(id: "low")),
            ]))"#,
    );

    let ui = compile_blocks(&resolver, "blocks.klayout.ron").unwrap();

    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected a module root");
    };
    let ExpandedNode::Column { children, .. } = &**root else {
        panic!("expected a column");
    };
    let ExpandedNode::Optional { block, child } = &children[0] else {
        panic!("expected an optional block");
    };

    assert_eq!(ui.resolve(block.path), "mixer/eq");
    assert_eq!(ui.resolve(block.hidden.id), "ui.block.hidden");
    assert!(matches!(**child, ExpandedNode::Control { .. }));
}

#[kithara::test]
fn a_layout_block_compiles_around_the_node_it_wraps() {
    let mut resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Knob(id: "low"))"#,
    );
    resolver.insert(
        "wrapped.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "wrapped",
            root: Split(axis: Horizontal, children: [
                (node: Optional(id: "sidebar", hidden: Model(id: "ui.block.hidden"),
                    node: Module(instance: "mixer", source: "blocks.kmodule.ron"))),
            ]))"#,
    );

    let ui = compile_blocks(&resolver, "wrapped.klayout.ron").unwrap();

    let CompiledNode::Split { children, .. } = &ui.root else {
        panic!("expected a split root");
    };
    let CompiledNode::Optional { block, child } = &children[0].1 else {
        panic!("expected an optional block");
    };

    assert_eq!(ui.resolve(block.path), "sidebar");
    assert_eq!(ui.resolve(block.hidden.id), "ui.block.hidden");
    assert!(matches!(**child, CompiledNode::Module { .. }));
}

#[kithara::test]
fn an_unknown_hidden_endpoint_is_rejected() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Model(id: "ui.block.absent"),
                    child: Knob(id: "low")),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::UnknownEndpoint { id, path, .. }
            if id == "ui.block.absent" && path == "mixer/eq"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_hidden_endpoint_that_is_not_a_bool_is_rejected() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Model(id: "deck.view.zoom"),
                    child: Knob(id: "low")),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == "Bool" && got == "Scalar" && path == "mixer/eq"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_command_endpoint_on_hidden_is_a_direction_error() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Command(id: "deck.view.zoom_in", with: { "deck": "a" }),
                    child: Knob(id: "low")),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::BindingDirection { id, .. } if id == "deck.view.zoom_in"),
        "{error:?}"
    );
}

#[kithara::test]
fn parameter_substitution_reaches_the_hidden_binding() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "decks.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "decks",
            root: Module(instance: "deck-b", source: "deck.kmodule.ron", with: { "deck": "b" }))"#,
    );
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
            root: Column(children: [
                Optional(id: "eq", hidden: Telemetry(id: "deck.playback.playing", with: { "deck": "$deck" }),
                    child: Knob(id: "low")),
            ]))"#,
    );

    let ui = compile_blocks(&resolver, "decks.klayout.ron").unwrap();

    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected a module root");
    };
    let ExpandedNode::Column { children, .. } = &**root else {
        panic!("expected a column");
    };
    let ExpandedNode::Optional { block, .. } = &children[0] else {
        panic!("expected an optional block");
    };

    assert_eq!(ui.resolve(block.path), "deck-b/eq");
    assert_eq!(ui.resolve(block.hidden.key), "deck.playback.playing@deck=b");
}

#[kithara::test]
fn an_optional_at_a_layout_root_is_rejected() {
    let mut resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Knob(id: "low"))"#,
    );
    resolver.insert(
        "root.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "root",
            root: Optional(id: "sidebar", hidden: Model(id: "ui.block.hidden"),
                node: Module(instance: "mixer", source: "blocks.kmodule.ron")))"#,
    );

    let error = compile_blocks(&resolver, "root.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::RootBlock { id, .. } if id == "sidebar"),
        "{error:?}"
    );
}

#[kithara::test]
fn an_optional_at_a_module_root_is_rejected() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                child: Knob(id: "low")))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::RootBlock { id, .. } if id == "eq"),
        "{error:?}"
    );
}

#[kithara::test]
fn an_optional_directly_under_an_optional_is_rejected() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                    child: Optional(id: "low", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "gain"))),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::RootBlock { id, .. } if id == "low"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_layout_optional_directly_under_an_optional_is_rejected() {
    let mut resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Knob(id: "low"))"#,
    );
    resolver.insert(
        "nested.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "nested",
            root: Split(axis: Horizontal, children: [
                (node: Optional(id: "outer", hidden: Model(id: "ui.block.hidden"),
                    node: Optional(id: "inner", hidden: Model(id: "ui.block.hidden"),
                        node: Module(instance: "mixer", source: "blocks.kmodule.ron")))),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "nested.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::RootBlock { id, .. } if id == "inner"),
        "{error:?}"
    );
}

#[kithara::test]
fn an_address_separator_in_a_block_prefix_is_rejected() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "prefixed.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "prefixed",
            root: Module(instance: "mixer.a", source: "blocks.kmodule.ron"))"#,
    );
    resolver.insert(
        "blocks.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Column(children: [
                Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                    child: Knob(id: "low")),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "prefixed.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::InvalidId { id, .. } if id == "mixer.a/eq"),
        "{error:?}"
    );
}

#[kithara::test]
fn an_unresolved_parameter_in_a_layout_block_binding_is_rejected() {
    let mut resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Knob(id: "low"))"#,
    );
    resolver.insert(
        "scoped.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "scoped",
            root: Split(axis: Horizontal, children: [
                (node: Optional(id: "sidebar",
                    hidden: Model(id: "ui.block.deck_hidden", with: { "deck": "$deck" }),
                    node: Module(instance: "mixer", source: "blocks.kmodule.ron"))),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "scoped.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::UnresolvedParam { name, .. } if name == "deck"),
        "{error:?}"
    );
}

const POPOVER_MODULE: &str = r#"(schema: "kithara.module", version: 1, id: "app-menu",
    root: Row(children: [
        Popover(
            id: "menu",
            open: Model(id: "ui.menu.open"),
            anchor: Pressable(
                id: "burger",
                press: Command(id: "ui.menu.toggle"),
                child: Row(id: "burger-cell", size: (w: Fixed(36.0), h: Fill), children: [
                    Glyph(id: "burger-icon", icon: Menu, style: MenuBurger, color: TextDim),
                ]),
            ),
            content: Column(id: "pop", size: (w: Fixed(298.0), h: Shrink), children: [
                Text(id: "brand", style: BrandSmall, label: "KITHARA"),
                Optional(id: "modules", hidden: Model(id: "ui.block.hidden"),
                    child: Text(id: "modules-label", style: Mono, color: Text, label: "MODULES")),
            ]),
        ),
    ]))"#;

#[kithara::test]
fn a_popover_compiles_with_its_anchor_and_its_content_subtree() {
    let resolver = block_resolver(POPOVER_MODULE);

    let ui = compile_blocks(&resolver, "blocks.klayout.ron").unwrap();

    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected a module root");
    };
    let ExpandedNode::Row { children, .. } = &**root else {
        panic!("expected a row");
    };
    let ExpandedNode::Popover {
        path,
        open,
        anchor,
        content,
        ..
    } = &children[0]
    else {
        panic!("expected a popover");
    };

    assert_eq!(ui.resolve(*path), "mixer/menu");
    assert_eq!(ui.resolve(open.id), "ui.menu.open");

    let ExpandedNode::Pressable { path, press, child } = &**anchor else {
        panic!("expected a pressable anchor");
    };
    assert_eq!(ui.resolve(*path), "mixer/burger");
    assert_eq!(ui.resolve(press.id), "ui.menu.toggle");
    assert!(matches!(**child, ExpandedNode::Row { .. }));

    let ExpandedNode::Column { children, .. } = &**content else {
        panic!("expected the pop column");
    };
    assert!(matches!(children[1], ExpandedNode::Optional { .. }));
}

#[kithara::test]
fn a_popover_carries_its_declared_geometry_into_the_compiled_tree() {
    let pointer = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "app-menu",
            root: Row(children: [
                Popover(id: "menu", open: Model(id: "ui.menu.open"), at: Pointer,
                    anchor: Row(id: "row", children: []),
                    content: Row(id: "pop", children: [])),
            ]))"#,
    );

    for (resolver, want) in [
        (block_resolver(POPOVER_MODULE), PopoverAt::Anchor),
        (pointer, PopoverAt::Pointer),
    ] {
        let ui = compile_blocks(&resolver, "blocks.klayout.ron").unwrap();
        let CompiledNode::Module { root, .. } = &ui.root else {
            panic!("expected a module root");
        };
        let ExpandedNode::Row { children, .. } = &**root else {
            panic!("expected a row");
        };
        let ExpandedNode::Popover { at, .. } = &children[0] else {
            panic!("expected a popover");
        };

        assert_eq!(*at, want);
    }
}

#[kithara::test]
fn a_popover_open_endpoint_must_be_a_bool() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "app-menu",
            root: Row(children: [
                Popover(id: "menu", open: Model(id: "deck.view.zoom"),
                    anchor: Row(id: "burger", children: []),
                    content: Row(id: "pop", children: [])),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == "Bool" && got == "Scalar" && path == "mixer/menu"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_popover_inside_another_popovers_content_is_rejected() {
    let direct = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "app-menu",
            root: Row(children: [
                Popover(id: "menu", open: Model(id: "ui.menu.open"),
                    anchor: Row(id: "burger", children: []),
                    content: Popover(id: "submenu", open: Model(id: "ui.menu.open"),
                        anchor: Row(id: "sub-anchor", children: []),
                        content: Row(id: "sub-pop", children: []))),
            ]))"#,
    );
    let mut included = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "app-menu",
            root: Row(children: [
                Popover(id: "menu", open: Model(id: "ui.menu.open"),
                    anchor: Row(id: "burger", children: []),
                    content: Include(id: "sub", source: "sub.kmodule.ron")),
            ]))"#,
    );
    included.insert(
        "sub.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "sub",
            root: Popover(id: "submenu", open: Model(id: "ui.menu.open"),
                anchor: Row(id: "sub-anchor", children: []),
                content: Row(id: "sub-pop", children: [])))"#,
    );

    for (resolver, nested) in [(direct, "mixer/submenu"), (included, "mixer/sub/submenu")] {
        let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

        assert!(
            matches!(&error, UiDocError::InvalidId { id, reason, .. }
                if id == nested && reason.contains("popover")),
            "{nested}: {error:?}"
        );
    }
}

#[kithara::test]
fn a_pressable_press_endpoint_must_be_a_trigger() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "app-menu",
            root: Row(children: [
                Pressable(id: "row", press: Parameter(id: "player.output.volume"),
                    child: Row(id: "inner", children: [])),
            ]))"#,
    );

    let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

    assert!(
        matches!(&error, UiDocError::BindingType { expected, got, path, .. }
            if expected == "Trigger" && got == "Scalar" && path == "mixer/row"),
        "{error:?}"
    );
}

#[kithara::test]
fn a_popover_takes_the_size_of_its_anchor_alone() {
    let resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "app-menu",
            root: Popover(id: "menu", open: Model(id: "ui.menu.open"),
                anchor: Row(id: "burger", size: (w: Fixed(36.0), h: Fixed(36.0)), children: []),
                content: Row(id: "pop", size: (w: Fixed(300.0), h: Fixed(400.0)), children: [])))"#,
    );

    let ui = compile_blocks(&resolver, "blocks.klayout.ron").unwrap();

    let CompiledNode::Module { size, .. } = &ui.root else {
        panic!("expected a module root");
    };
    assert_eq!(*size, SizeSpec::new(Dim::Fixed(36.0), Dim::Fixed(36.0)));
}

#[kithara::test]
fn an_active_binding_on_a_row_or_a_glyph_must_be_a_bool() {
    let cases = [
        (
            r#"Row(id: "row", active: Model(id: "deck.view.zoom"), children: [])"#,
            "mixer/row",
        ),
        (
            r#"Row(active: Model(id: "deck.view.zoom"), children: [])"#,
            "mixer",
        ),
        (
            r#"Glyph(id: "icon", icon: Menu, style: Menu, active: Model(id: "deck.view.zoom"))"#,
            "mixer/icon",
        ),
    ];

    for (node, at) in cases {
        let resolver = block_resolver(&format!(
            r#"(schema: "kithara.module", version: 1, id: "app-menu",
                root: Column(children: [{node}]))"#
        ));

        let error = compile_blocks(&resolver, "blocks.klayout.ron").unwrap_err();

        assert!(
            matches!(&error, UiDocError::BindingType { expected, got, path, .. }
                if expected == "Bool" && got == "Scalar" && path == at),
            "{node}: {error:?}"
        );
    }
}

#[kithara::test]
fn an_escaped_dollar_in_a_layout_block_binding_stays_a_literal() {
    let mut resolver = block_resolver(
        r#"(schema: "kithara.module", version: 1, id: "mixer",
            root: Knob(id: "low"))"#,
    );
    resolver.insert(
        "escaped.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "escaped",
            root: Split(axis: Horizontal, children: [
                (node: Optional(id: "sidebar",
                    hidden: Model(id: "ui.block.deck_hidden", with: { "deck": "$$deck" }),
                    node: Module(instance: "mixer", source: "blocks.kmodule.ron"))),
            ]))"#,
    );

    let ui = compile_blocks(&resolver, "escaped.klayout.ron").unwrap();

    let CompiledNode::Split { children, .. } = &ui.root else {
        panic!("expected a split root");
    };
    let CompiledNode::Optional { block, .. } = &children[0].1 else {
        panic!("expected an optional block");
    };

    assert_eq!(
        ui.resolve(block.hidden.key),
        "ui.block.deck_hidden@deck=$deck"
    );
}

fn glyph_resolver(row: &str) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "menu.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "menu",
            root: Module(instance: "menu", source: "menu.kmodule.ron"))"#,
    );
    resolver.insert(
        "menu.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "menu",
            root: Row(children: [
                Include(id: "one", source: "row.kmodule.ron", with: { "glyph": "Monitor" }),
                Include(id: "two", source: "row.kmodule.ron", with: { "glyph": "Disc" }),
            ]))"#,
    );
    resolver.insert("row.kmodule.ron", row);
    resolver
}

fn compile_glyphs(resolver: &MemResolver) -> Result<CompiledUi, UiDocError> {
    compile(
        "menu.klayout.ron",
        resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
}

fn glyph_icons(ui: &CompiledUi) -> Vec<IconName> {
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Row { children, .. } = &**root else {
        panic!("expected a row root");
    };
    children
        .iter()
        .map(|child| {
            let ExpandedNode::Control {
                spec: ControlSpec::Glyph { icon, .. },
                ..
            } = child
            else {
                panic!("expected a glyph");
            };
            *icon
        })
        .collect()
}

#[kithara::test]
fn one_template_carries_a_different_glyph_per_include() {
    let ui = compile_glyphs(&glyph_resolver(
        r#"(schema: "kithara.module", version: 1, id: "row", parameters: ["glyph"],
            root: Glyph(id: "icon", icon: "$glyph"))"#,
    ))
    .unwrap();

    assert_eq!(glyph_icons(&ui), [IconName::Monitor, IconName::Disc]);
}

#[kithara::test]
fn a_misspelled_icon_is_rejected_and_never_read_as_a_parameter() {
    let error = compile_glyphs(&glyph_resolver(
        r#"(schema: "kithara.module", version: 1, id: "row", parameters: ["glyph"],
            root: Glyph(id: "icon", icon: Loudspeaker))"#,
    ))
    .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("Loudspeaker"), "{message}");
}

#[kithara::test]
fn an_argument_that_names_no_icon_is_rejected_where_it_is_spent() {
    let mut resolver = glyph_resolver(
        r#"(schema: "kithara.module", version: 1, id: "row", parameters: ["glyph"],
            root: Glyph(id: "icon", icon: "$glyph"))"#,
    );
    resolver.insert(
        "menu.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "menu",
            root: Row(children: [
                Include(id: "one", source: "row.kmodule.ron", with: { "glyph": "Trombone" }),
            ]))"#,
    );

    let error = compile_glyphs(&resolver).unwrap_err();

    let message = error.to_string();
    assert!(message.contains("Trombone"), "{message}");
    assert!(message.contains("menu/one/icon"), "{message}");
}

#[kithara::test]
fn one_template_reads_a_different_endpoint_per_include() {
    let mut resolver = glyph_resolver(
        r#"(schema: "kithara.module", version: 1, id: "row", parameters: ["endpoint"],
            root: Glyph(id: "icon", icon: Disc, active: Model(id: "$endpoint")))"#,
    );
    resolver.insert(
        "menu.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "menu",
            root: Row(children: [
                Include(id: "one", source: "row.kmodule.ron", with: { "endpoint": "ui.prefs.mono" }),
                Include(id: "two", source: "row.kmodule.ron", with: { "endpoint": "ui.prefs.autogain" }),
            ]))"#,
    );
    let mut registry = common::player_registry();
    for id in ["ui.prefs.mono", "ui.prefs.autogain"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool),
        );
    }

    let ui = compile(
        "menu.klayout.ron",
        &resolver,
        &registry,
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap();

    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Row { children, .. } = &**root else {
        panic!("expected a row root");
    };
    let keys: Vec<_> = children
        .iter()
        .map(|child| {
            let ExpandedNode::Control {
                spec:
                    ControlSpec::Glyph {
                        active: Some(active),
                        ..
                    },
                ..
            } = child
            else {
                panic!("expected a glyph with an active binding");
            };
            ui.resolve(active.key)
        })
        .collect();

    assert_eq!(keys, ["ui.prefs.mono", "ui.prefs.autogain"]);
}

#[kithara::test]
fn a_template_may_not_read_an_endpoint_the_registry_does_not_know() {
    let mut resolver = glyph_resolver(
        r#"(schema: "kithara.module", version: 1, id: "row", parameters: ["endpoint"],
            root: Glyph(id: "icon", icon: Disc, active: Model(id: "$endpoint")))"#,
    );
    resolver.insert(
        "menu.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "menu",
            root: Row(children: [
                Include(id: "one", source: "row.kmodule.ron", with: { "endpoint": "ui.prefs.nope" }),
            ]))"#,
    );

    let error = compile_glyphs(&resolver).unwrap_err();

    assert!(
        matches!(error, UiDocError::UnknownEndpoint { .. }),
        "{error}"
    );
}
