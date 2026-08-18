use crate::{
    compile::{CompiledNode, compiled_node_size, module_size},
    layout::Axis,
    size::{Hidden, SizeSpec, combine_horizontal, combine_vertical, is_hidden},
    skin::SkinDoc,
};

pub(super) fn node_size(node: &CompiledNode, skin: &SkinDoc, hidden: Hidden<'_>) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => node_size(child, skin, hidden),
        node if !node.blocks() => compiled_node_size(node),
        CompiledNode::Split { axis, children, .. } => {
            let sizes =
                visible_children(children, hidden).map(|(_, child)| node_size(child, skin, hidden));
            match axis {
                Axis::Horizontal => combine_horizontal(sizes),
                Axis::Vertical => combine_vertical(sizes),
            }
        }
        CompiledNode::Module { chrome, root, .. } => module_size(root, *chrome, skin, hidden),
    }
}

pub(super) fn visible_children<'a>(
    children: &'a [(f32, CompiledNode)],
    hidden: Hidden<'a>,
) -> impl Iterator<Item = (f32, &'a CompiledNode)> {
    children
        .iter()
        .filter(move |(_, child)| !is_hidden(child, hidden))
        .map(|(weight, child)| (*weight, child))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        compile::{CompiledUi, compile},
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
        size::{Dim, VISIBLE},
        source::{MemResolver, UiConfig},
    };

    struct Registry(EndpointDesc);

    impl Default for Registry {
        fn default() -> Self {
            Self(EndpointDesc::new(ValueKind::Bool))
        }
    }

    impl EndpointRegistry for Registry {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            match (category, id.0.as_str()) {
                (EndpointCategory::Model, "ui.block.hidden") => Some(&self.0),
                _ => None,
            }
        }
    }

    fn compiled(module: &str) -> CompiledUi {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "blocks.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "blocks",
                root: Module(instance: "mixer", source: "blocks.kmodule.ron"))"#,
        );
        resolver.insert("blocks.kmodule.ron", module);
        compile(
            "blocks.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap()
    }

    fn size_of(ui: &CompiledUi, hidden: Hidden<'_>) -> SizeSpec {
        node_size(&ui.root, builtin::skin_doc(), hidden)
    }

    const HIDDEN: Hidden<'static> = &|_| true;

    #[kithara::test]
    fn a_hidden_block_leaves_the_module_the_size_it_has_without_it() {
        let full = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 0.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                    Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "low")),
                ]))"#,
        );
        let trimmed = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 0.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                ]))"#,
        );

        assert_eq!(size_of(&full, HIDDEN), size_of(&trimmed, VISIBLE));
        assert_ne!(
            size_of(&full, VISIBLE),
            size_of(&trimmed, VISIBLE),
            "a visible block takes space",
        );
    }

    #[kithara::test]
    fn a_hidden_block_takes_its_gap_with_it() {
        let full = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 9.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                    Knob(id: "trim"),
                    Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "low")),
                ]))"#,
        );
        let trimmed = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Row(gap: 9.0, pad: 0.0, children: [
                    Knob(id: "volume"),
                    Knob(id: "trim"),
                ]))"#,
        );

        assert_eq!(size_of(&full, HIDDEN), size_of(&trimmed, VISIBLE));
    }

    #[kithara::test]
    fn a_slot_whose_only_child_is_hidden_fills_like_an_empty_one() {
        let full = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Slot(id: "extra", default: [
                    Optional(id: "eq", hidden: Model(id: "ui.block.hidden"),
                        child: Knob(id: "low")),
                ]))"#,
        );
        let empty = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Slot(id: "extra"))"#,
        );

        assert_eq!(size_of(&full, HIDDEN), size_of(&empty, VISIBLE));
        assert_ne!(size_of(&full, VISIBLE), size_of(&empty, VISIBLE));
    }

    #[kithara::test]
    fn a_split_whose_children_are_all_hidden_folds_to_nothing() {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "blocks.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Knob(id: "low"))"#,
        );
        resolver.insert(
            "split.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "split",
                root: Split(axis: Horizontal, children: [
                    (node: Optional(id: "left", hidden: Model(id: "ui.block.hidden"),
                        node: Module(instance: "a", source: "blocks.kmodule.ron"))),
                    (node: Optional(id: "right", hidden: Model(id: "ui.block.hidden"),
                        node: Module(instance: "b", source: "blocks.kmodule.ron"))),
                ]))"#,
        );
        let ui = compile(
            "split.klayout.ron",
            &resolver,
            &Registry::default(),
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
        )
        .unwrap();

        assert_eq!(
            size_of(&ui, HIDDEN),
            SizeSpec::new(Dim::Fixed(0.0), Dim::Fixed(0.0)),
        );
        assert_ne!(size_of(&ui, VISIBLE), size_of(&ui, HIDDEN));
    }
}
