use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    expand::{
        Binding, BlockSpec, Budget, ControlSite, DropSpec, ExpandedNode, Expander, intern_binding,
        substitute_binding, substitute_map,
    },
    ids::{InternId, Interner, SourceUri, StrArena},
    layout::{Axis, FrameSides, LayoutNode, parse_layout},
    module::ChromeStyle,
    registry::EndpointRegistry,
    resolve::load_module_graph,
    size::{
        BlockNode, Hidden, SizeSpec, VISIBLE, combine_horizontal, combine_vertical, compute_size,
        has_blocks, is_hidden, with_module_chrome,
    },
    skin::SkinDoc,
    source::{SourceResolver, UiConfig},
    validate,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CompiledUi {
    pub root: CompiledNode,
    pub size: SizeSpec,
    /// The layout asked to be framed by its own resize edges.
    pub resize_edges: bool,
    /// Names the item the pointer is carrying; drawn at the pointer.
    pub dragged: Option<Binding>,
    arena: StrArena,
}

impl CompiledUi {
    delegate::delegate! {
        to self.arena {
            /// Resolves a string interned by this compiled UI.
            #[must_use]
            pub fn resolve(&self, id: InternId) -> &str;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CompiledNode {
    Split {
        axis: Axis,
        children: Vec<(f32, Self)>,
        size: SizeSpec,
        /// An optional block sits below, so `size` holds only while every one
        /// of them reads visible.
        blocks: bool,
    },
    Optional {
        block: BlockSpec,
        child: Box<Self>,
    },
    Module {
        instance: InternId,
        module: InternId,
        title: Option<InternId>,
        chip: Option<InternId>,
        assign: Vec<InternId>,
        chrome: ChromeStyle,
        frame: FrameSides,
        corners: bool,
        footer: Option<Binding>,
        drop: Option<DropSpec>,
        collapsed: InternId,
        root: Box<ExpandedNode>,
        size: SizeSpec,
        /// An optional block sits below, so `size` holds only while every one
        /// of them reads visible.
        blocks: bool,
    },
}

impl CompiledNode {
    /// Whether this node's recorded `size` can change with the snapshot.
    const fn blocks(&self) -> bool {
        match self {
            Self::Split { blocks, .. } | Self::Module { blocks, .. } => *blocks,
            Self::Optional { .. } => true,
        }
    }
}

impl BlockNode for CompiledNode {
    fn block(&self) -> Option<&BlockSpec> {
        match self {
            Self::Optional { block, .. } => Some(block),
            _ => None,
        }
    }
}

/// Compiles a layout and its module graph into renderer-ready UI data.
///
/// # Errors
/// Returns [`UiDocError`] when loading, parsing, expansion, or validation fails.
pub fn compile(
    entry: &str,
    resolver: &dyn SourceResolver,
    endpoints: &dyn EndpointRegistry,
    skin: &SkinDoc,
    config: &UiConfig,
) -> Result<CompiledUi, UiDocError> {
    let loaded = resolver.load(None, entry)?;
    let bytes = loaded.text.len();
    if bytes > config.limits.max_bytes {
        return Err(UiDocError::TooLarge {
            origin: loaded.uri,
            bytes,
            max: config.limits.max_bytes,
        });
    }
    let document = parse_layout(&loaded.text, &loaded.uri)?;
    validate::check_layout_instances(&document, &loaded.uri)?;
    validate::check_layout_dragged(&document, &loaded.uri, endpoints)?;
    let mut budget = Budget::new(config.limits.max_nodes);
    let mut interner = Interner::new(config.max_arena_bytes);
    let root = Compiler {
        resolver,
        endpoints,
        skin,
        config,
        budget: &mut budget,
        interner: &mut interner,
    }
    .build(&document.root, &loaded.uri)?;
    let size = compiled_node_size(&root);
    let dragged = document
        .dragged
        .as_ref()
        .map(|binding| intern_binding(&mut interner, binding, &loaded.uri))
        .transpose()?;
    let arena = interner.finish();
    Ok(CompiledUi {
        root,
        size,
        resize_edges: document.resize_edges,
        dragged,
        arena,
    })
}

struct Compiler<'a> {
    resolver: &'a dyn SourceResolver,
    endpoints: &'a dyn EndpointRegistry,
    skin: &'a SkinDoc,
    config: &'a UiConfig,
    budget: &'a mut Budget,
    interner: &'a mut Interner,
}

impl Compiler<'_> {
    fn build(
        &mut self,
        node: &LayoutNode,
        layout_uri: &SourceUri,
    ) -> Result<CompiledNode, UiDocError> {
        self.budget.charge(layout_uri)?;
        match node {
            LayoutNode::Split { axis, children } => {
                let children: Vec<_> = children
                    .iter()
                    .map(|child| Ok((child.weight, self.build(&child.node, layout_uri)?)))
                    .collect::<Result<_, UiDocError>>()?;
                let sizes = children.iter().map(|(_, child)| compiled_node_size(child));
                let size = match axis {
                    Axis::Horizontal => combine_horizontal(sizes),
                    Axis::Vertical => combine_vertical(sizes),
                };
                let blocks = children.iter().any(|(_, child)| child.blocks());
                Ok(CompiledNode::Split {
                    axis: *axis,
                    children,
                    size,
                    blocks,
                })
            }
            LayoutNode::Optional { id, hidden, node } => {
                let hidden = substitute_binding(&no_args(), layout_uri, hidden, &id.0)?;
                validate::check_layout_block(&hidden, &id.0, layout_uri, self.endpoints)?;
                let child = self.build(node, layout_uri)?;
                Ok(CompiledNode::Optional {
                    block: BlockSpec {
                        path: self.interner.intern(&id.0, layout_uri)?,
                        hidden: intern_binding(self.interner, &hidden, layout_uri)?,
                    },
                    child: Box::new(child),
                })
            }
            LayoutNode::Module {
                instance,
                source,
                with,
                size,
                frame,
                corners,
            } => {
                let args = substitute_map(&no_args(), layout_uri, with, &instance.0)?;
                let (module_uri, set) = load_module_graph(
                    self.resolver,
                    Some(layout_uri),
                    source,
                    &self.config.limits,
                )?;
                let mut visitor = |site: ControlSite<'_>, origin: &SourceUri| {
                    validate::check_controls(site, origin, self.endpoints)
                };
                let document = set
                    .defs
                    .get(&module_uri)
                    .ok_or_else(|| UiDocError::NotFound {
                        origin: module_uri.clone(),
                        rel: module_uri.0.clone(),
                    })?;
                validate::check_module_footer(document, &module_uri, self.endpoints)?;
                validate::check_module_drop(document, &module_uri, self.endpoints)?;
                let expanded = Expander::new(
                    self.config.limits.max_depth,
                    self.budget,
                    self.interner,
                    &mut visitor,
                )
                .expand_module(&set, &module_uri, &args, &instance.0)?;
                let declared = *size;
                let size = declared.unwrap_or_else(|| {
                    module_size(&expanded.root, expanded.chrome, self.skin, VISIBLE)
                });
                let blocks = declared.is_none() && has_blocks(&expanded.root);
                let instance = self.interner.intern(&instance.0, layout_uri)?;
                Ok(CompiledNode::Module {
                    instance,
                    module: expanded.module,
                    title: expanded.title,
                    chip: expanded.chip,
                    assign: expanded.assign,
                    chrome: expanded.chrome,
                    frame: *frame,
                    corners: *corners,
                    footer: expanded.footer,
                    drop: expanded.drop,
                    collapsed: expanded.collapsed,
                    root: Box::new(expanded.root),
                    size,
                    blocks,
                })
            }
        }
    }
}

fn compiled_node_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => compiled_node_size(child),
        CompiledNode::Split { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}

fn module_size(
    root: &ExpandedNode,
    chrome: ChromeStyle,
    skin: &SkinDoc,
    hidden: Hidden<'_>,
) -> SizeSpec {
    with_module_chrome(compute_size(root, skin, hidden), chrome, skin)
}

/// Intrinsic size of a compiled node under a visibility snapshot. A subtree
/// that carries no optional block returns the size recorded for it at compile
/// time.
pub(crate) fn node_size(node: &CompiledNode, skin: &SkinDoc, hidden: Hidden<'_>) -> SizeSpec {
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

/// The split children a layout lays out; a hidden block claims no share.
pub(crate) fn visible_children<'a>(
    children: &'a [(f32, CompiledNode)],
    hidden: Hidden<'a>,
) -> impl Iterator<Item = (f32, &'a CompiledNode)> {
    children
        .iter()
        .filter(move |(_, child)| !is_hidden(child, hidden))
        .map(|(weight, child)| (*weight, child))
}

/// A layout declares no parameters, so every `$name` it writes is unresolvable.
fn no_args() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, ValueKind},
        size::Dim,
        source::MemResolver,
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
