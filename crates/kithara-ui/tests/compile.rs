mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    error::UiDocError,
    expand::{Binding, ControlSpec, ExpandedNode},
    module::{ChromeStyle, TrackColumn},
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
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Control {
        spec: ControlSpec::Crossfader,
        read: Some(Binding::Parameter { .. }),
        write: Some(Binding::Parameter { .. }),
        ..
    } = &**root
    else {
        panic!("expected compiled crossfader");
    };
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
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Control {
        spec: ControlSpec::TrackList {
            columns,
            columns_state,
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
    let Some(Binding::Model { id, .. }) = columns_state else {
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
                corners: false,
            ))"#,
    );
    resolver.insert(
        "shell.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
            title: Some("Deck"), chip: Some("DECK"), chrome: Full,
            footer: Some(Telemetry(id: "deck.track.title", with: { "deck": "$deck" })),
            root: Text(id: "label"))"#,
    );

    let ui = compile(
        "shell.klayout.ron",
        &resolver,
        &common::player_registry(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module {
        module,
        title,
        chip,
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
    assert_eq!(*chrome, ChromeStyle::Full);
    assert!(frame.top);
    assert!(!frame.right);
    assert!(frame.bottom);
    assert!(!frame.left);
    assert!(!corners);
    assert_eq!(ui.resolve(*collapsed), "ui.module.deck.collapsed");
    let Some(Binding::Telemetry { id, with }) = footer else {
        panic!("expected telemetry footer");
    };
    assert_eq!(ui.resolve(*id), "deck.track.title");
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
