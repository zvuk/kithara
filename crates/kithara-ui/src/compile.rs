use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    expand::{
        Binding, BlockSpec, Budget, ControlSite, DropSpec, ExpandedNode, Expander, intern_binding,
        substitute_binding, substitute_map,
    },
    ids::{InternId, Interner, SourceUri, StrArena},
    layout::{Axis, FrameSides, LayoutNode, SplitChild, parse_layout},
    module::{ChromeStyle, MeasureAxis},
    registry::EndpointRegistry,
    resolve::load_module_graph,
    room,
    size::{
        BlockNode, Cell, Cells, DEFAULTS, SizeSpec, Snapshot, at_least, axis_min,
        combine_horizontal, combine_vertical, compute_size, has_blocks, min_size,
        with_module_chrome,
    },
    skin::SkinDoc,
    source::{SourceResolver, UiConfig},
    text::TextDoc,
    validate::{self, NodePath},
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CompiledUi {
    pub root: CompiledNode,
    /// Names the item the pointer is carrying; drawn at the pointer.
    pub dragged: Option<Binding>,
    pub size: SizeSpec,
    /// The room the whole tree needs, which is the smallest window it draws in.
    pub min: SizeSpec,
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
        /// The axis the split reads to decide which of its cells stand.
        measure: Option<MeasureAxis>,
        children: Vec<SplitCell>,
        /// The box the layout declares for the split.
        size: Option<SizeSpec>,
        /// What its cells compose to, which is the box it shows its parent
        /// while it declares none of its own.
        composed: SizeSpec,
        blocks: bool,
    },
    Optional {
        block: BlockSpec,
        child: Box<Self>,
    },
    /// Lays out the branch that fits the room it is given.
    Adaptive {
        axis: MeasureAxis,
        size: SizeSpec,
        base: Box<Self>,
        steps: Vec<(f32, Self)>,
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

/// One cell of a split: the node, the share of the room it takes among the
/// cells standing beside it, and the band of room it stands in.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SplitCell {
    pub node: CompiledNode,
    pub weight: f32,
    pub from: f32,
    pub until: Option<f32>,
}

impl CompiledNode {
    pub(crate) const fn blocks(&self) -> bool {
        match self {
            Self::Split { blocks, .. } | Self::Module { blocks, .. } => *blocks,
            Self::Optional { .. } => true,
            Self::Adaptive { .. } => false,
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
    let min = compiled_min(&root, skin);
    let dragged = document
        .dragged
        .as_ref()
        .map(|binding| intern_binding(&mut interner, binding, &loaded.uri))
        .transpose()?;
    let arena = interner.finish();
    Ok(CompiledUi {
        root,
        size,
        min,
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
            LayoutNode::Split {
                axis,
                measure,
                size,
                children,
            } => self.build_split(*axis, *measure, *size, children, layout_uri),
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
            LayoutNode::Adaptive {
                id,
                measure,
                size,
                base,
                steps,
            } => {
                validate::check_layout_measure(id, *measure, *size, layout_uri)?;
                let base = self.build(base, layout_uri)?;
                let steps: Vec<_> = steps
                    .iter()
                    .map(|step| Ok((step.from, self.build(&step.node, layout_uri)?)))
                    .collect::<Result<_, UiDocError>>()?;
                room::check_layout_steps(id, *measure, &steps, self.skin, layout_uri)?;
                room::check_box(
                    Some(*size),
                    compiled_min(&base, self.skin),
                    &NodePath::default().push(format!("Adaptive({id})")),
                    layout_uri,
                )?;
                Ok(CompiledNode::Adaptive {
                    steps,
                    axis: *measure,
                    size: *size,
                    base: Box::new(base),
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
                room::check_module(&expanded.root, self.skin, &module_uri)?;
                let declared = *size;
                room::check_box(
                    declared,
                    with_module_chrome(
                        min_size(&expanded.root, self.skin),
                        expanded.chrome,
                        self.skin,
                    ),
                    &NodePath::default().push(format!("Module({instance})")),
                    layout_uri,
                )?;
                let size = declared.unwrap_or_else(|| {
                    module_size(&expanded.root, expanded.chrome, self.skin, DEFAULTS)
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

    /// A split answers for the box it declares and for the room its cells
    /// settle on, so both checks run here rather than at the parent holding
    /// it.
    fn build_split(
        &mut self,
        axis: Axis,
        measure: Option<MeasureAxis>,
        size: Option<SizeSpec>,
        children: &[SplitChild],
        layout_uri: &SourceUri,
    ) -> Result<CompiledNode, UiDocError> {
        let children: Vec<_> = children
            .iter()
            .map(|child| {
                Ok(SplitCell {
                    node: self.build(&child.node, layout_uri)?,
                    weight: child.weight,
                    from: child.from,
                    until: child.until,
                })
            })
            .collect::<Result<Vec<_>, UiDocError>>()?;
        let sizes = children.iter().map(|cell| compiled_node_size(&cell.node));
        let composed = match axis {
            Axis::Horizontal => combine_horizontal(sizes),
            Axis::Vertical => combine_vertical(sizes),
        };
        let path = NodePath::default().push("Split");
        let cells = split_cells(axis, &children, self.skin);
        let needed = cells.settled(measure);
        if let Some(measure) = measure {
            room::check_layout_cells(
                &cells,
                measure,
                axis_min(at_least(size, needed), measure),
                &path,
                layout_uri,
            )?;
        }
        room::check_box(size, needed, &path, layout_uri)?;
        let blocks = size.is_none() && children.iter().any(|cell| cell.node.blocks());
        Ok(CompiledNode::Split {
            axis,
            measure,
            children,
            size,
            composed,
            blocks,
        })
    }
}

pub(crate) fn compiled_node_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => compiled_node_size(child),
        CompiledNode::Split { size, composed, .. } => size.unwrap_or(*composed),
        CompiledNode::Adaptive { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}

/// The cells a compiled split lays out, each with the band it stands in and
/// the room the node behind it needs.
fn split_cells(axis: Axis, children: &[SplitCell], skin: &SkinDoc) -> Cells {
    Cells::new(
        axis,
        children
            .iter()
            .map(|cell| Cell::new(cell.from, cell.until, compiled_min(&cell.node, skin)))
            .collect(),
    )
}

/// The room one branch of a compiled tree needs, which is what a threshold
/// standing that branch has to promise.
#[must_use]
pub fn compiled_min(node: &CompiledNode, skin: &SkinDoc) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => compiled_min(child, skin),
        CompiledNode::Adaptive { size, base, .. } => {
            at_least(Some(*size), compiled_min(base, skin))
        }
        CompiledNode::Split {
            axis,
            measure,
            children,
            size,
            ..
        } => at_least(*size, split_cells(*axis, children, skin).settled(*measure)),
        CompiledNode::Module {
            root, chrome, size, ..
        } => at_least(
            Some(*size),
            with_module_chrome(min_size(root, skin), *chrome, skin),
        ),
    }
}

pub(crate) fn module_size(
    root: &ExpandedNode,
    chrome: ChromeStyle,
    skin: &SkinDoc,
    snapshot: &dyn Snapshot,
) -> SizeSpec {
    with_module_chrome(compute_size(root, skin, snapshot), chrome, skin)
}
