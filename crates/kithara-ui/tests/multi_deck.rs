mod common;

use std::collections::BTreeMap;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi, compile},
    expand::{Binding, ExpandedNode},
    ids::InternId,
    source::{MemResolver, UiConfig},
};

fn resolver_with(fixture_name: &str, text: &str) -> MemResolver {
    let mut resolver = builtin::resolver();
    resolver.insert(fixture_name, text);
    resolver
}

fn collect_instances(ui: &CompiledUi, node: &CompiledNode, instances: &mut Vec<String>) {
    match node {
        CompiledNode::Split { children, .. } => {
            for (_, child) in children {
                collect_instances(ui, child, instances);
            }
        }
        CompiledNode::Module { instance, .. } => {
            instances.push(ui.resolve(*instance).to_owned());
        }
        _ => {}
    }
}

fn binding_scopes(binding: &Binding) -> Option<&BTreeMap<InternId, InternId>> {
    match binding {
        Binding::Command { with, .. }
        | Binding::Parameter { with, .. }
        | Binding::Telemetry { with, .. }
        | Binding::Model { with, .. } => Some(with),
        _ => None,
    }
}

fn collect_control_decks(ui: &CompiledUi, node: &ExpandedNode, decks: &mut Vec<String>) {
    match node {
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Slot { children, .. } => {
            for child in children {
                collect_control_decks(ui, child, decks);
            }
        }
        ExpandedNode::Control { read, write, .. } => {
            for binding in read.iter().chain(write.iter()) {
                if let Some(deck) = binding_scopes(binding).and_then(|with| {
                    with.iter()
                        .find(|(key, _)| ui.resolve(**key) == "deck")
                        .map(|(_, value)| ui.resolve(*value))
                }) {
                    decks.push(deck.to_owned());
                }
            }
        }
        _ => {}
    }
}

fn collect_instance_decks(
    ui: &CompiledUi,
    node: &CompiledNode,
    decks: &mut BTreeMap<String, Vec<String>>,
) {
    match node {
        CompiledNode::Split { children, .. } => {
            for (_, child) in children {
                collect_instance_decks(ui, child, decks);
            }
        }
        CompiledNode::Module { instance, root, .. } => {
            let mut values = Vec::new();
            collect_control_decks(ui, root, &mut values);
            decks.insert(ui.resolve(*instance).to_owned(), values);
        }
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
        &common::player_registry(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let mut instances = Vec::new();
    collect_instances(&ui, &ui.root, &mut instances);
    instances.sort();
    assert_eq!(instances, ["deck-a", "deck-b", "deck-c", "deck-d"]);

    let mut decks = BTreeMap::new();
    collect_instance_decks(&ui, &ui.root, &mut decks);
    for (instance, expected) in [
        ("deck-a", "a"),
        ("deck-b", "b"),
        ("deck-c", "c"),
        ("deck-d", "d"),
    ] {
        let values = &decks[instance];
        assert!(!values.is_empty(), "{instance} has no scoped bindings");
        assert!(
            values.iter().all(|value| value == expected),
            "{instance} scopes: {values:?}"
        );
    }
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
        &common::player_registry(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap();
}
