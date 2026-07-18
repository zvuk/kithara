mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    source::{Limits, MemResolver},
};

fn resolver_with(fixture_name: &str, text: &str) -> MemResolver {
    let mut resolver = builtin::resolver();
    resolver.insert(fixture_name, text);
    resolver
}

fn collect_instances(node: &CompiledNode, instances: &mut Vec<String>) {
    match node {
        CompiledNode::Split { children, .. } => {
            for (_, child) in children {
                collect_instances(child, instances);
            }
        }
        CompiledNode::Module { instance, .. } => instances.push(instance.0.clone()),
        _ => {}
    }
}

#[kithara::test]
fn four_deck_layout_instantiates_one_module_file_four_times() {
    let resolver = resolver_with(
        "four_deck.klayout.ron",
        include_str!("fixtures/four_deck.klayout.ron"),
    );
    let ui = compile(
        "four_deck.klayout.ron",
        &resolver,
        &common::player_catalog(),
        &common::player_registry(&["a", "b", "c", "d"]),
        &Limits::default(),
    )
    .unwrap();
    let mut instances = Vec::new();
    collect_instances(&ui.root, &mut instances);
    instances.sort();
    assert_eq!(instances, ["deck-a", "deck-b", "deck-c", "deck-d"]);
}

#[kithara::test]
fn two_deck_layout_compiles() {
    let resolver = resolver_with(
        "two_deck.klayout.ron",
        include_str!("fixtures/two_deck.klayout.ron"),
    );
    compile(
        "two_deck.klayout.ron",
        &resolver,
        &common::player_catalog(),
        &common::player_registry(&["a", "b"]),
        &Limits::default(),
    )
    .unwrap();
}
