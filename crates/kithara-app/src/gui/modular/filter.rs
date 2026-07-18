use std::collections::BTreeSet;

use kithara_ui::compile::CompiledNode;

pub(super) fn visible(node: &CompiledNode, hidden: &BTreeSet<String>) -> Option<CompiledNode> {
    match node {
        CompiledNode::Split { axis, children } => {
            let children = children
                .iter()
                .filter_map(|(weight, child)| visible(child, hidden).map(|child| (*weight, child)))
                .collect::<Vec<_>>();
            (!children.is_empty()).then_some(CompiledNode::Split {
                children,
                axis: *axis,
            })
        }
        CompiledNode::Module { instance, .. } if hidden.contains(&instance.0) => None,
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
        source::Limits,
    };

    use super::visible;
    use crate::gui::modular::endpoints;

    fn compile_player() -> CompiledUi {
        compile(
            builtin::PLAYER_PRESET,
            &builtin::resolver(),
            &endpoints::catalog(),
            &endpoints::registry(),
            &Limits::default(),
        )
        .unwrap_or_else(|error| panic!("player preset must compile: {error}"))
    }

    #[kithara::test]
    fn hides_one_of_two_modules() {
        let compiled = compile_player();
        let hidden = BTreeSet::from(["library".to_owned()]);
        let filtered = visible(&compiled.root, &hidden)
            .unwrap_or_else(|| panic!("deck module must remain visible"));

        let CompiledNode::Split { children, .. } = filtered else {
            panic!("player root must remain a split");
        };
        assert_eq!(children.len(), 1);
        assert!(matches!(
            &children[0].1,
            CompiledNode::Module { instance, .. } if instance.0 == "deck-a"
        ));
    }

    #[kithara::test]
    fn hiding_all_modules_removes_the_tree() {
        let compiled = compile_player();
        let hidden = BTreeSet::from(["deck-a".to_owned(), "library".to_owned()]);

        assert!(visible(&compiled.root, &hidden).is_none());
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
            &endpoints::catalog(),
            &endpoints::registry(),
            &Limits::default(),
        )
        .unwrap_or_else(|error| panic!("nested layout must compile: {error}"));
        let filtered = visible(&compiled.root, &BTreeSet::from(["deck-a".to_owned()]))
            .unwrap_or_else(|| panic!("library module must remain visible"));

        let CompiledNode::Split { children, .. } = filtered else {
            panic!("outer split must remain");
        };
        assert_eq!(children.len(), 1);
        assert!(matches!(
            &children[0].1,
            CompiledNode::Module { instance, .. } if instance.0 == "library"
        ));
    }
}
