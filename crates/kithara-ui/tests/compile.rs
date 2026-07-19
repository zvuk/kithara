mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    error::UiDocError,
    expand::ExpandedNode,
    module::PropValue,
    size::{Dim, SizeSpec},
    source::{Limits, MemResolver, UiConfig},
};

fn resolver() -> MemResolver {
    builtin::resolver()
}

#[kithara::test]
fn compiles_micro_layout_end_to_end() {
    let ui = compile(
        "micro.klayout.ron",
        &resolver(),
        &common::player_catalog(),
        &common::player_registry(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { instance, .. } = &ui.root else {
        panic!("expected module root");
    };
    assert_eq!(ui.resolve(*instance), "deck-a");
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
            root: Control(id: "play", kind: "button"))"#,
    );

    let ui = compile(
        "override.klayout.ron",
        &resolver,
        &common::player_catalog(),
        &common::player_registry(),
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
fn unknown_endpoint_fails_with_module_origin_and_path() {
    let mut resolver = resolver();
    resolver.insert(
        "modules/deck/transport.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "transport", parameters: ["deck"],
            root: Control(id: "play", kind: "button",
                write: Command(id: "deck.transport.typo", with: { "deck": "$deck" })))"#,
    );
    let error = compile(
        "player.klayout.ron",
        &resolver,
        &common::player_catalog(),
        &common::player_registry(),
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
        &common::player_catalog(),
        &common::player_registry(),
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
            root: Control(id: "title", kind: "text"))"#,
    );

    let error = compile(
        "layout.klayout.ron",
        &resolver,
        &common::player_catalog(),
        &common::player_registry(),
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
            root: Control(id: "text", kind: "text", props: {
                "style": Text("$x"),
            }))"#,
    );

    let ui = compile(
        "literal.klayout.ron",
        &resolver,
        &common::player_catalog(),
        &common::player_registry(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module");
    };
    let ExpandedNode::Control { props, .. } = &**root else {
        panic!("expected control");
    };
    let value = props
        .iter()
        .find(|(key, _)| ui.resolve(**key) == "style")
        .map(|(_, value)| value);
    let Some(PropValue::Text(value)) = value else {
        panic!("expected text prop");
    };
    assert_eq!(ui.resolve(*value), "$lit");
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
        &common::player_catalog(),
        &common::player_registry(),
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
        &common::player_catalog(),
        &common::player_registry(),
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
