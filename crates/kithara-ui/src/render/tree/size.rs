use crate::{
    compile::{CompiledNode, compiled_node_size, module_size},
    layout::Axis,
    size::{SizeSpec, Snapshot, combine_horizontal, combine_vertical, is_hidden},
    skin::SkinDoc,
};

pub(super) fn node_size(node: &CompiledNode, skin: &SkinDoc, snapshot: &dyn Snapshot) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => node_size(child, skin, snapshot),
        node if !node.blocks() => compiled_node_size(node),
        CompiledNode::Split { axis, children, .. } => {
            let sizes = visible_children(children, snapshot)
                .map(|(_, child)| node_size(child, skin, snapshot));
            match axis {
                Axis::Horizontal => combine_horizontal(sizes),
                Axis::Vertical => combine_vertical(sizes),
            }
        }
        CompiledNode::Module { chrome, root, .. } => module_size(root, *chrome, skin, snapshot),
    }
}

pub(super) fn visible_children<'a>(
    children: &'a [(f32, CompiledNode)],
    snapshot: &'a dyn Snapshot,
) -> impl Iterator<Item = (f32, &'a CompiledNode)> {
    children
        .iter()
        .filter(move |(_, child)| !is_hidden(child, snapshot))
        .map(|(weight, child)| (*weight, child))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        compile::{CompiledUi, compile},
        expand::{Binding, BlockSpec},
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
        size::{DEFAULTS, Dim},
        source::{MemResolver, UiConfig},
    };

    struct Registry {
        flag: EndpointDesc,
        scalar: EndpointDesc,
        trigger: EndpointDesc,
    }

    impl Default for Registry {
        fn default() -> Self {
            Self {
                flag: EndpointDesc::new(ValueKind::Bool),
                scalar: EndpointDesc::new(ValueKind::Scalar),
                trigger: EndpointDesc::new(ValueKind::Trigger),
            }
        }
    }

    impl EndpointRegistry for Registry {
        fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
            match (category, id.0.as_str()) {
                (EndpointCategory::Model, "ui.block.hidden") => Some(&self.flag),
                (EndpointCategory::Model, "ui.measure") => Some(&self.scalar),
                (EndpointCategory::Command, "ui.press") => Some(&self.trigger),
                _ => None,
            }
        }
    }

    struct AllHidden;

    impl Snapshot for AllHidden {
        fn hidden(&self, _: &BlockSpec) -> bool {
            true
        }

        fn measure(&self, _: &Binding) -> Option<f32> {
            None
        }
    }

    struct Measured(f32);

    impl Snapshot for Measured {
        fn hidden(&self, _: &BlockSpec) -> bool {
            false
        }

        fn measure(&self, _: &Binding) -> Option<f32> {
            Some(self.0)
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

    fn size_of(ui: &CompiledUi, snapshot: &dyn Snapshot) -> SizeSpec {
        node_size(&ui.root, builtin::skin_doc(), snapshot)
    }

    const HIDDEN: &dyn Snapshot = &AllHidden;

    #[kithara::test]
    fn an_adaptive_bank_is_the_size_of_the_branch_its_measure_selects() {
        let ui = compiled(
            r#"(schema: "kithara.module", version: 1, id: "mixer",
                root: Popover(
                    id: "menu",
                    open: Model(id: "ui.block.hidden"),
                    anchor: Pressable(
                        id: "press",
                        press: Command(id: "ui.press"),
                        child: Adaptive(
                            id: "bank",
                            measure: Read(Model(id: "ui.measure")),
                            base: Row(id: "narrow", gap: 0.0, pad: 0.0, children: [
                                Knob(id: "low"),
                            ]),
                            steps: [
                                (from: 4.0, node: Row(id: "wide", gap: 0.0, pad: 0.0, children: [
                                    Knob(id: "low-4"),
                                    Knob(id: "high-4"),
                                ])),
                            ],
                        ),
                    ),
                    content: Knob(id: "pop"),
                ))"#,
        );

        let three = size_of(&ui, &Measured(3.0));
        let four = size_of(&ui, &Measured(4.0));

        assert_ne!(three, four, "each branch measures for itself");
        assert_eq!(
            size_of(&ui, DEFAULTS),
            three,
            "an unread measure takes base"
        );
        assert!(four.w.min() > three.w.min());
    }

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

        assert_eq!(size_of(&full, HIDDEN), size_of(&trimmed, DEFAULTS));
        assert_ne!(
            size_of(&full, DEFAULTS),
            size_of(&trimmed, DEFAULTS),
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

        assert_eq!(size_of(&full, HIDDEN), size_of(&trimmed, DEFAULTS));
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

        assert_eq!(size_of(&full, HIDDEN), size_of(&empty, DEFAULTS));
        assert_ne!(size_of(&full, DEFAULTS), size_of(&empty, DEFAULTS));
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
        assert_ne!(size_of(&ui, DEFAULTS), size_of(&ui, HIDDEN));
    }
}
