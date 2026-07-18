mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    error::UiDocError,
    source::{Limits, MemResolver},
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
        &common::player_registry(&["a"]),
        &Limits::default(),
    )
    .unwrap();
    let CompiledNode::Module { instance, .. } = &ui.root else {
        panic!("expected module root");
    };
    assert_eq!(instance.0, "deck-a");
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
        "micro.klayout.ron",
        &resolver,
        &common::player_catalog(),
        &common::player_registry(&["a"]),
        &Limits::default(),
    )
    .unwrap_err();
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
        &common::player_registry(&["a"]),
        &limits,
    )
    .unwrap_err();
    assert!(matches!(error, UiDocError::NodesExceeded { max: 1, .. }));
}
