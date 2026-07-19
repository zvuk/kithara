use std::collections::BTreeSet;

use kithara_ui::compile::{CompiledNode, CompiledUi};

pub(super) fn visible(
    node: &CompiledNode,
    hidden: &BTreeSet<String>,
    ui: &CompiledUi,
) -> Option<CompiledNode> {
    match node {
        CompiledNode::Split {
            axis,
            children,
            size,
        } => {
            let children = children
                .iter()
                .filter_map(|(weight, child)| {
                    visible(child, hidden, ui).map(|child| (*weight, child))
                })
                .collect::<Vec<_>>();
            (!children.is_empty()).then_some(CompiledNode::Split {
                children,
                axis: *axis,
                size: *size,
            })
        }
        CompiledNode::Module { instance, .. } if hidden.contains(ui.resolve(*instance)) => None,
        _ => Some(node.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kithara_test_utils::kithara;
    use kithara_ui::{
        builtin,
        compile::{CompiledNode, CompiledUi, compile},
        render::tree::catalog,
        source::UiConfig,
    };

    use super::visible;
    use crate::gui::modular::endpoints;

    fn compile_player() -> CompiledUi {
        compile(
            builtin::PLAYER_PRESET,
            &builtin::resolver(),
            &catalog(),
            &endpoints::registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("player preset must compile: {error}"))
    }

    #[kithara::test]
    fn hides_one_module_keeps_the_rest() {
        let compiled = compile_player();
        let hidden = BTreeSet::from(["library".to_owned()]);
        let filtered = visible(&compiled.root, &hidden, &compiled)
            .unwrap_or_else(|| panic!("deck module must remain visible"));

        let CompiledNode::Split { children, .. } = filtered else {
            panic!("player root must remain a split");
        };
        let names: Vec<_> = children
            .iter()
            .filter_map(|(_, child)| match child {
                CompiledNode::Module { instance, .. } => Some(compiled.resolve(*instance)),
                _ => None,
            })
            .collect();
        assert!(!names.contains(&"library"), "library must be hidden");
        assert!(names.contains(&"deck-a"), "deck must remain visible");
    }

    #[kithara::test]
    fn hiding_all_modules_removes_the_tree() {
        let compiled = compile_player();
        let hidden = BTreeSet::from([
            "global-bar".to_owned(),
            "deck-a".to_owned(),
            "library".to_owned(),
        ]);

        assert!(visible(&compiled.root, &hidden, &compiled).is_none());
    }

    #[kithara::test]
    fn removes_empty_nested_splits() {
        let mut resolver = builtin::resolver();
        resolver.insert(
            "nested.klayout.ron",
            r#"(
                schema: "kithara.layout",
                version: 1,
                id: "nested",
                root: Split(
                    axis: Horizontal,
                    children: [
                        (weight: 1.0, node: Split(
                            axis: Vertical,
                            children: [
                                (weight: 1.0, node: Module(
                                    instance: "deck-a",
                                    source: "modules/deck.kmodule.ron",
                                    with: { "deck": "a" },
                                )),
                            ],
                        )),
                        (weight: 1.0, node: Module(
                            instance: "library",
                            source: "modules/library.kmodule.ron",
                        )),
                    ],
                ),
            )"#,
        );
        let compiled = compile(
            "nested.klayout.ron",
            &resolver,
            &catalog(),
            &endpoints::registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("nested layout must compile: {error}"));
        let filtered = visible(
            &compiled.root,
            &BTreeSet::from(["deck-a".to_owned()]),
            &compiled,
        )
        .unwrap_or_else(|| panic!("library module must remain visible"));

        let CompiledNode::Split { children, .. } = filtered else {
            panic!("outer split must remain");
        };
        assert_eq!(children.len(), 1);
        assert!(matches!(
            &children[0].1,
            CompiledNode::Module { instance, .. } if compiled.resolve(*instance) == "library"
        ));
    }
}
