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
        has_blocks, with_module_chrome,
    },
    skin::SkinDoc,
    source::{SourceResolver, UiConfig},
    text::TextDoc,
    validate,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CompiledUi {
    pub root: CompiledNode,
    /// Names the item the pointer is carrying; drawn at the pointer.
    pub dragged: Option<Binding>,
    pub size: SizeSpec,
    /// The layout asked to be framed by its own resize edges.
    pub resize_edges: bool,
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
        blocks: bool,
    },
}

impl CompiledNode {
    pub(crate) const fn blocks(&self) -> bool {
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
    text: &TextDoc,
    config: &UiConfig,
) -> Result<CompiledUi, UiDocError> {
    let loaded = resolver.load(None, entry)?;
    let bytes = loaded.text.len();
    if bytes > config.limits.max_bytes {
        return Err(UiDocError::TooLarge {
            bytes,
            origin: loaded.uri,
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
        text,
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
        dragged,
        arena,
        resize_edges: document.resize_edges,
    })
}

struct Compiler<'a> {
    budget: &'a mut Budget,
    interner: &'a mut Interner,
    skin: &'a SkinDoc,
    text: &'a TextDoc,
    config: &'a UiConfig,
    endpoints: &'a dyn EndpointRegistry,
    resolver: &'a dyn SourceResolver,
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
                    children,
                    size,
                    blocks,
                    axis: *axis,
                })
            }
            LayoutNode::Optional { id, hidden, node } => {
                let hidden = substitute_binding(&BTreeMap::new(), layout_uri, hidden, &id.0)?;
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
                let args = substitute_map(&BTreeMap::new(), layout_uri, with, &instance.0)?;
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
                    self.text,
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
                    size,
                    blocks,
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
                })
            }
        }
    }
}

pub(crate) fn compiled_node_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => compiled_node_size(child),
        CompiledNode::Split { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}

pub(crate) fn module_size(
    root: &ExpandedNode,
    chrome: ChromeStyle,
    skin: &SkinDoc,
    hidden: Hidden<'_>,
) -> SizeSpec {
    with_module_chrome(compute_size(root, skin, hidden), chrome, skin)
}
