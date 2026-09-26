use std::collections::BTreeMap;

#[cfg(feature = "render")]
use crate::draw::{DrawBuffers, PoolStats};
use crate::{
    error::UiDocError,
    expand::{
        Binding, BlockSpec, Budget, ControlSite, DropSpec, ExpandedInclude, ExpandedNode, Expander,
        Unprompted, intern_binding, motion_of, scoped_state, substitute_binding, substitute_map,
    },
    ids::{InstanceId, InternId, Interner, SourceUri, StrArena},
    layout::{Axis, FrameCorners, FrameSides, LayoutNode, SplitChild, parse_layout},
    module::{ChromeStyle, MeasureAxis},
    registry::{BuiltinEndpoints, EndpointRegistry},
    require,
    resolve::load_module_graph,
    room,
    shader::ShaderCache,
    size::{
        BlockNode, Cell, Cells, SizeSpec, Snapshot, at_least, axis_min, combine_horizontal,
        combine_vertical, compute_size, consts::DEFAULTS, has_blocks, min_size, with_module_chrome,
    },
    skin::SkinDoc,
    source::{SourceResolver, UiConfig},
    text::TextDoc,
    validate::{self, NodePath},
    view::{Census, Side, Tabs as ViewTabs, ViewState, ViewWrites},
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
    /// Somewhere in this document an object is placed by an endpoint, so
    /// re-reading the endpoints can move something a host already mounted.
    pub driven: bool,
    /// This document draws a different picture at a later moment with nothing
    /// else changing: an object is placed by an endpoint, or a binding reads the
    /// host's own clock.
    ///
    /// A host that stops drawing such a document animates it only while some
    /// unrelated event keeps waking it — a mouse crossing the window, and
    /// nothing once the mouse stops.
    pub animates: bool,
    /// Where each press of this screen writes the screen's own state.
    ///
    /// The host answers these itself: a document that turns its own state says
    /// so here, and the application is neither asked nor required to have
    /// declared anything for it.
    views: ViewWrites,
    arena: StrArena,
    includes: Vec<IncludedModule>,
    #[cfg(feature = "render")]
    draw_buffers: DrawBuffers,
}

impl CompiledUi {
    #[cfg(feature = "render")]
    #[must_use]
    pub(crate) const fn draw_buffers(&self) -> &DrawBuffers {
        &self.draw_buffers
    }

    /// Current allocation-reuse counters for this compiled document.
    #[cfg(feature = "render")]
    #[must_use]
    pub fn draw_pool_stats(&self) -> PoolStats {
        self.draw_buffers.stats()
    }

    #[cfg(feature = "render")]
    pub(crate) fn includes_module(
        &self,
        owner: InternId,
        address: &Address<'_>,
        module: &str,
    ) -> bool {
        self.includes
            .iter()
            .filter(|include| include.owner == owner && address.names(&include.address))
            .any(|include| self.resolve(include.module) == module)
    }

    /// Refuses a screen that answers on none of the paths an application binds
    /// behaviour to.
    ///
    /// An application reaches its own interface by path: a press arrives
    /// named, and the application decides what it means. A package free to lay
    /// its screens out as it likes is therefore also free to lay out one that
    /// answers nowhere, and nothing about drawing it would say so - the window
    /// would open with no way to start playing. Naming the few paths without
    /// which the application is not itself turns that into a refusal.
    ///
    /// Popover, pressable and control paths all count: they are what a host
    /// addresses, and an application binds to whichever of them its documents
    /// use.
    ///
    /// # Errors
    /// Returns [`UiDocError::MissingPaths`] listing every required path the
    /// screen does not answer on.
    pub fn require_paths(&self, required: &[&str], origin: &SourceUri) -> Result<(), UiDocError> {
        let missing = require::missing(self, required);
        if missing.is_empty() {
            return Ok(());
        }
        Err(UiDocError::MissingPaths {
            origin: origin.clone(),
            paths: missing,
        })
    }

    /// Where each press of this screen writes its own state.
    #[must_use]
    pub const fn views(&self) -> &ViewWrites {
        &self.views
    }

    delegate::delegate! {
        to self.arena {
            /// Resolves a string interned by this compiled UI.
            #[must_use]
            pub fn resolve(&self, id: InternId) -> &str;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct IncludedModule {
    address: Box<[usize]>,
    module: InternId,
    owner: InternId,
}

/// Where one node sits under the document root.
///
/// A walk carries its position by borrowing the parent it came from rather
/// than owning a path of its own: the address is read at the few leaves that
/// ask whether they host an engine, and nothing about it outlives the mount
/// that built it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Address<'a> {
    Root,
    Child { parent: &'a Self, index: usize },
}

impl Address<'_> {
    /// The address one step further down, at `index` among this node's children.
    pub(crate) const fn child(&self, index: usize) -> Address<'_> {
        Address::Child {
            index,
            parent: self,
        }
    }

    /// Whether this address names `path`, compared from the leaf upward.
    fn names(&self, path: &[usize]) -> bool {
        let mut node = self;
        let mut rest = path;
        while let Self::Child { parent, index } = node {
            let Some((last, head)) = rest.split_last() else {
                return false;
            };
            if last != index {
                return false;
            }
            node = parent;
            rest = head;
        }
        rest.is_empty()
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
        /// The window corners this module stands at, filled in once the whole
        /// layout is built.
        round: FrameCorners,
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
    pub until: Option<f32>,
    pub from: f32,
    pub weight: f32,
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
///
/// Layers over the application's own declarations, so a document may bind to what the host answers
/// for itself without every application registering it.
pub fn compile(
    entry: &str,
    resolver: &dyn SourceResolver,
    endpoints: &dyn EndpointRegistry,
    skin: &SkinDoc,
    text: &TextDoc,
    config: &UiConfig,
    view: &ViewState,
) -> Result<CompiledUi, UiDocError> {
    let endpoints = &BuiltinEndpoints::new(endpoints);
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
    let mut includes = Vec::new();
    let mut shaders = ShaderCache::default();
    let mut states = Census::default();
    let mut root = Compiler {
        resolver,
        endpoints,
        skin,
        text,
        config,
        view,
        budget: &mut budget,
        interner: &mut interner,
        includes: &mut includes,
        shaders: &mut shaders,
        states: &mut states,
    }
    .build(&document.root, &loaded.uri)?;
    round_corners(&mut root, FrameCorners::ALL);
    let size = compiled_node_size(&root);
    let min = compiled_min(&root, skin);
    let dragged = document
        .dragged
        .as_ref()
        .map(|binding| intern_binding(&mut interner, binding, &loaded.uri))
        .transpose()?;
    let unprompted = motion_of_layout(&root);
    let driven = unprompted.driven;
    let animates = driven || unprompted.continuous || interner.reads_clock();
    let views = states.finish()?;
    let arena = interner.finish();
    Ok(CompiledUi {
        root,
        size,
        min,
        dragged,
        animates,
        driven,
        includes,
        arena,
        views,
        resize_edges: document.resize_edges,
        #[cfg(feature = "render")]
        draw_buffers: config.draw_buffers.clone(),
    })
}

/// Hands every module the window corners it stands at.
///
/// The root of a layout is the window, so it owns all four; a split keeps the
/// pair across its axis whole for every cell and gives the pair along it to the
/// cells at its two ends. A branch that fills its parent's box - an optional
/// block, an adaptive step - inherits what its parent was given. The corners a
/// module ends up with are the ones its own frame radius may round, which is
/// what makes the window round rather than the boxes inside it.
///
/// The end of a split is where the document puts it. A cell the host hides at
/// runtime does not move the corner to its neighbour.
fn round_corners(node: &mut CompiledNode, corners: FrameCorners) {
    match node {
        CompiledNode::Module { round, .. } => *round = corners,
        CompiledNode::Optional { child, .. } => round_corners(child, corners),
        CompiledNode::Adaptive { base, steps, .. } => {
            round_corners(base, corners);
            for (_, step) in steps {
                round_corners(step, corners);
            }
        }
        CompiledNode::Split { axis, children, .. } => {
            let axis = *axis;
            let last = children.len().saturating_sub(1);
            for (index, cell) in children.iter_mut().enumerate() {
                round_corners(&mut cell.node, cell_corners(axis, corners, index, last));
            }
        }
    }
}

/// The corners one cell of a split inherits: the whole pair across the axis,
/// and the pair along it only at the end the cell stands at.
const fn cell_corners(
    axis: Axis,
    corners: FrameCorners,
    index: usize,
    last: usize,
) -> FrameCorners {
    let (start, end) = match axis {
        Axis::Horizontal => (corners.left(), corners.right()),
        Axis::Vertical => (corners.top(), corners.bottom()),
    };
    match (index == 0, index == last) {
        (true, true) => corners,
        (true, false) => start,
        (false, true) => end,
        (false, false) => FrameCorners::EMPTY,
    }
}

/// Where one module stands in a layout: the same placement whether the
/// document named the module itself or a page of a `Tabs`.
#[derive(Clone, Copy)]
struct ModuleAt<'a> {
    with: &'a BTreeMap<String, String>,
    instance: &'a InstanceId,
    source: &'a str,
    frame: FrameSides,
    size: Option<SizeSpec>,
    corners: bool,
}

struct Compiler<'a> {
    budget: &'a mut Budget,
    states: &'a mut Census,
    interner: &'a mut Interner,
    shaders: &'a mut ShaderCache,
    skin: &'a SkinDoc,
    text: &'a TextDoc,
    config: &'a UiConfig,
    includes: &'a mut Vec<IncludedModule>,
    view: &'a ViewState,
    endpoints: &'a dyn EndpointRegistry,
    resolver: &'a dyn SourceResolver,
}

impl Compiler<'_> {
    /// Names each state at the scope where it is declared: at the top of the document when no
    /// module instance contains it, or scoped to the screen when the layout owns every instance.
    /// Only the page the screen currently stands at is compiled.
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
                let hidden = substitute_binding(&BTreeMap::new(), layout_uri, hidden, &id.0, "")?;
                validate::check_layout_block(&hidden, &id.0, layout_uri, self.endpoints)?;
                self.states.note(&id.0, &hidden, layout_uri, Side::Read);
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
            } => self.build_module(
                ModuleAt {
                    instance,
                    source,
                    with,
                    size: *size,
                    frame: *frame,
                    corners: *corners,
                },
                layout_uri,
            ),
            LayoutNode::Tabs {
                state,
                initial,
                pages,
            } => {
                let path = NodePath::default().push(format!("Tabs({state})")).render();
                let state = scoped_state("", &state.0);
                let standing = self.view.page(&state).unwrap_or(initial);
                let node = pages.get(standing).ok_or_else(|| UiDocError::UnknownPage {
                    origin: layout_uri.clone(),
                    id: state.clone(),
                    page: standing.to_owned(),
                    path: path.clone(),
                })?;
                self.states.note_pages(ViewTabs {
                    initial,
                    origin: layout_uri,
                    pages: pages.keys().cloned().collect(),
                    path: &path,
                    shown: standing,
                    state: &state,
                });
                self.build(node, layout_uri)
            }
        }
    }

    fn build_module(
        &mut self,
        at: ModuleAt<'_>,
        layout_uri: &SourceUri,
    ) -> Result<CompiledNode, UiDocError> {
        let args = substitute_map(&BTreeMap::new(), layout_uri, at.with, &at.instance.0)?;
        let (module_uri, set) = load_module_graph(
            self.resolver,
            Some(layout_uri),
            at.source,
            &self.config.limits,
        )?;
        let endpoints = self.endpoints;
        let kinds = &self.config.custom_kinds;
        let states = &mut *self.states;
        let mut visitor = |site: ControlSite<'_>, origin: &SourceUri| {
            states.note_site(site, origin);
            validate::check_controls(site, origin, endpoints, kinds)
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
        let mut expanded = Expander::new(
            self.config.limits.max_depth,
            self.budget,
            self.interner,
            self.endpoints,
            self.shaders,
            self.text,
            &mut visitor,
        )
        .expand_module(&set, &module_uri, &args, &at.instance.0)?;
        room::check_module(&expanded.root, self.skin, &module_uri)?;
        let declared = at.size;
        room::check_box(
            declared,
            with_module_chrome(
                min_size(&expanded.root, self.skin),
                expanded.chrome,
                self.skin,
            ),
            &NodePath::default().push(format!("Module({})", at.instance)),
            layout_uri,
        )?;
        let size = declared
            .unwrap_or_else(|| module_size(&expanded.root, expanded.chrome, self.skin, DEFAULTS));
        let blocks = declared.is_none() && has_blocks(&expanded.root);
        let instance = self.interner.intern(&at.instance.0, layout_uri)?;
        self.includes.extend(
            std::mem::take(&mut expanded.includes)
                .into_iter()
                .map(|include| included_module(instance, include)),
        );
        Ok(CompiledNode::Module {
            instance,
            size,
            blocks,
            module: expanded.module,
            title: expanded.title,
            chip: expanded.chip,
            assign: expanded.assign,
            chrome: expanded.chrome,
            frame: at.frame,
            corners: at.corners,
            round: FrameCorners::EMPTY,
            footer: expanded.footer,
            drop: expanded.drop,
            collapsed: expanded.collapsed,
            root: Box::new(expanded.root),
        })
    }

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

/// What every module of this layout does with nothing touching it.
///
/// Which branch stands is settled by the room this walk cannot see, so the layout's motion folds
/// over every branch rather than just the base.
fn motion_of_layout(node: &CompiledNode) -> Unprompted {
    match node {
        CompiledNode::Split { children, .. } => children
            .iter()
            .map(|cell| motion_of_layout(&cell.node))
            .fold(Unprompted::default(), Unprompted::or),
        CompiledNode::Optional { child, .. } => motion_of_layout(child),
        CompiledNode::Adaptive { base, steps, .. } => steps
            .iter()
            .map(|(_, branch)| motion_of_layout(branch))
            .fold(motion_of_layout(base), Unprompted::or),
        CompiledNode::Module { root, .. } => motion_of(root),
    }
}

fn included_module(owner: InternId, include: ExpandedInclude) -> IncludedModule {
    IncludedModule {
        owner,
        address: include.address,
        module: include.module,
    }
}

pub(crate) fn compiled_node_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Optional { child, .. } => compiled_node_size(child),
        CompiledNode::Split { size, composed, .. } => size.unwrap_or(*composed),
        CompiledNode::Adaptive { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}

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
