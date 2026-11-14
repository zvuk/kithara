use std::{cell::Cell, marker::PhantomData, rc::Rc};

use kithara_test_macros as kithara;
use masonry::{
    core::{NewWidget, Widget, WidgetId, WidgetPod},
    kurbo::Rect as MasonryRect,
};

use super::{
    mount::{NodeLayout, declared},
    spot::Spot,
};
use crate::{
    backends::affine,
    draw::{Pt, Rgba, Transform},
    expand::Binding,
    hosts::{hosted::HostedControlPlan, solve},
    ids::InternId,
    layout::{FrameCorners, FrameSides},
    masonry::{
        hosted::MasonryHostedState,
        retained::{
            custom::HostAction,
            menu::PickerLayer,
            node::{Detent, Face, Faces, Node},
            picker::{EngineTarget, HostedEngine},
            popover::{PopoverState, SurfaceKind},
        },
    },
    render::{Published, Skin},
    size::SizeSpec,
};

/// Whether the document hides one block right now.
///
/// The flow that holds the block reads this at layout, and the root writes it
/// when the document says otherwise: the block itself is mounted either way, so
/// nothing below it is rebuilt when it comes and goes.
#[derive(Default)]
pub(crate) struct BlockState {
    hidden: Cell<bool>,
}

impl BlockState {
    /// Records what the document says now, answering whether that is news.
    pub(crate) fn latch(&self, hidden: bool) -> bool {
        let changed = self.hidden.get() != hidden;
        self.hidden.set(hidden);
        changed
    }

    delegate::delegate! {
        to self.hidden {
            /// Whether the document hides the block right now.
            #[call(get)]
            pub(crate) fn is_hidden(&self) -> bool;
        }
    }
}

/// A node's box: fixed at mount, or its stage's box at layout.
#[derive(Clone)]
pub(crate) enum Natural {
    Fixed(solve::Size<solve::Length>),
    Stage(Rc<StageSize>),
}

impl Natural {
    pub(crate) fn now(&self) -> solve::Size<solve::Length> {
        match self {
            Self::Fixed(size) => *size,
            Self::Stage(stage) => stage.now(),
        }
    }
}

impl From<solve::Size<solve::Length>> for Natural {
    fn from(size: solve::Size<solve::Length>) -> Self {
        Self::Fixed(size)
    }
}

/// One child of a stage: the block hiding it, whether it floats above the
/// stage, and its box.
type StageChild<T = Natural> = (Option<Rc<BlockState>>, bool, T);

pub(crate) struct StageSize {
    size: Option<SizeSpec>,
    children: Vec<StageChild>,
}

impl StageSize {
    pub(crate) const fn new(size: Option<SizeSpec>, children: Vec<StageChild>) -> Self {
        Self { size, children }
    }

    pub(crate) fn shown(&self) -> impl Iterator<Item = bool> + '_ {
        self.children
            .iter()
            .map(|(block, _, _)| !block.as_ref().is_some_and(|block| block.is_hidden()))
    }

    /// Whether each child is shown and takes room in the stage.
    pub(crate) fn in_flow(&self) -> impl Iterator<Item = bool> + '_ {
        self.children
            .iter()
            .zip(self.shown())
            .map(|((_, floats, _), shown)| shown && !floats)
    }

    pub(crate) fn now(&self) -> solve::Size<solve::Length> {
        let first = self
            .children
            .iter()
            .zip(self.in_flow())
            .find_map(|((_, _, natural), on)| on.then(|| natural.now()));
        self.size
            .map(declared)
            .or(first)
            .unwrap_or(declared(SizeSpec::FILL))
    }
}

/// The blocks a mounted thing stands inside; any hidden one leaves it unread.
#[derive(Default)]
pub(crate) struct Enclosing(Vec<Rc<BlockState>>);

impl Enclosing {
    pub(crate) fn shown(&self) -> bool {
        self.0.iter().all(|block| !block.is_hidden())
    }
}

/// One registration and the blocks it stands inside.
pub(crate) struct Within<T> {
    pub(crate) item: T,
    pub(crate) within: Enclosing,
}

impl<T> From<T> for Within<T> {
    fn from(item: T) -> Self {
        Self {
            item,
            within: Enclosing::default(),
        }
    }
}

/// What the root reads again on every refresh, registered as the tree is built.
#[derive(Default)]
pub(crate) struct Registrations {
    pub(crate) watched: Vec<Within<Watched>>,
    pub(crate) blocks: Vec<Within<BlockRegistration>>,
    pub(crate) popovers: Vec<Within<PopoverRegistration>>,
    pub(crate) engines: Vec<Within<Rc<HostedEngine>>>,
    pub(crate) engine_targets: Vec<Within<EngineTarget>>,
}

impl Registrations {
    pub(crate) fn extend(&mut self, other: Self) {
        self.watched.extend(other.watched);
        self.blocks.extend(other.blocks);
        self.popovers.extend(other.popovers);
        self.engines.extend(other.engines);
        self.engine_targets.extend(other.engine_targets);
    }

    fn stands_in(&mut self, block: &Rc<BlockState>) {
        let within = self
            .watched
            .iter_mut()
            .map(|watched| &mut watched.within)
            .chain(self.blocks.iter_mut().map(|inner| &mut inner.within))
            .chain(self.popovers.iter_mut().map(|popover| &mut popover.within))
            .chain(self.engines.iter_mut().map(|engine| &mut engine.within))
            .chain(
                self.engine_targets
                    .iter_mut()
                    .map(|target| &mut target.within),
            );
        for enclosing in within {
            enclosing.0.push(Rc::clone(block));
        }
    }

    fn hides(&mut self, flow: WidgetId, blocks: Vec<(Binding, Rc<BlockState>)>) {
        self.blocks
            .extend(blocks.into_iter().map(|(hidden, state)| {
                BlockRegistration {
                    hidden,
                    state,
                    flow,
                }
                .into()
            }));
    }

    fn watch(&mut self, watched: Watched) {
        self.watched.push(watched.into());
    }
}

/// One block the tree mounted, and what a root needs to keep it in step with
/// the document it came from.
pub(crate) struct BlockRegistration {
    /// What the document reads to know the block is hidden.
    pub(crate) hidden: Binding,
    pub(crate) state: Rc<BlockState>,
    /// The flow that hides it, which is the node whose layout has to run again
    /// once the answer changes.
    pub(crate) flow: WidgetId,
}

/// One popover the tree mounted, and what a root needs to keep it in step with
/// the document it came from.
///
/// A retained tree mounts the surface whether the document holds it open or
/// shut, because the shape it mounts is the shape it keeps. So the flag is kept
/// beside it: the surface opening is not a value inside the content, it is
/// whether the content stands in the picture at all.
pub(crate) struct PopoverRegistration {
    /// What the document reads to know whether the surface stands open.
    pub(crate) flag: Binding,
    pub(crate) dismiss: Rc<dyn Fn() -> HostAction>,
    pub(crate) state: Rc<PopoverState>,
    /// The engine-driven controls the open surface answers for: the anchor it
    /// opens from, and everything the surface itself holds.
    ///
    /// An engine answers the pointer against its own box and cannot see what
    /// stands above it, while the document lays out siblings the open surface
    /// hangs across. So the surface names the controls it covers the room for.
    /// The anchor is one of them because a surface is closed by the control
    /// that opened it, and a surface wide enough covers that control too.
    pub(crate) controls: Vec<WidgetId>,
    /// The node the surface opens from.
    pub(crate) anchor: WidgetId,
    /// The layer the surface is drawn in.
    pub(crate) layer: WidgetId,
}

/// The window layer one tree mounted, and what a root needs to keep it in step
/// with the document it came from.
pub(crate) struct WindowTracker {
    pub(crate) layer: Option<WidgetId>,
    pub(crate) pointer: Rc<Cell<Option<Pt>>>,
    /// Whether the last reading found anything, which is when the layer has to
    /// be painted again as the pointer moves.
    pub(crate) carrying: bool,
    pub(crate) drops: bool,
}
/// One mounted leaf and the document source it re-reads without rebuilding.
pub(crate) enum Watched {
    Read {
        id: WidgetId,
        binding: Binding,
    },
    Snapshot {
        id: WidgetId,
    },
    /// A leaf some object places. Its pose comes from the document walk rather
    /// than from an endpoint of its own, so it is re-read by path.
    Placed {
        id: WidgetId,
        path: InternId,
    },
    /// A placement of a stage whose point an endpoint answers. The point moves
    /// the box the child stands in, so the answer is a layout rather than a
    /// repaint.
    Spot {
        id: WidgetId,
        binding: Binding,
    },
    /// A flow or a run of text that shows another face while the flag it names
    /// reads true. What a flag lights is a value, so the face is swapped into
    /// the node standing rather than settled where the tree is built.
    Lit {
        id: WidgetId,
        flag: Binding,
    },
    /// Content the application installed at a path, repainted when the
    /// application says the state it shows changed.
    Custom {
        id: WidgetId,
        path: String,
    },
    Zone {
        id: WidgetId,
        path: String,
    },
}
/// Where one node stands, as the root reads it out of the tree.
///
/// A mounted surface that answers a hand - a control an engine drives, a
/// window layer - needs the box its node stands in, and that box moves
/// without the node being told: Masonry recomputes a whole subtree itself
/// when a window above it scrolls and calls no widget back. So the node
/// carries a cell instead of a box, and the root fills it.
pub(crate) struct NodeBox {
    pub(crate) area: Rc<Cell<MasonryRect>>,
    pub(crate) node: WidgetId,
}

pub(crate) type LayerParts = (
    NewWidget<Node>,
    solve::Size<solve::Length>,
    Vec<NewWidget<dyn Widget>>,
    Registrations,
    Vec<NodeBox>,
    Vec<WidgetId>,
    Option<WindowTracker>,
);
pub(crate) type RootParts = (
    NewWidget<dyn Widget>,
    Vec<NewWidget<dyn Widget>>,
    Registrations,
    Vec<NodeBox>,
    Vec<WidgetId>,
    Option<WindowTracker>,
);

/// A retained Masonry tree produced by the document facade.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct MasonryNode<Action> {
    widget: NewWidget<Node>,
    #[field(get(deref = false), vis = "pub(crate)")]
    natural: Natural,
    action: PhantomData<fn() -> Action>,
    layers: Vec<NewWidget<dyn Widget>>,
    registrations: Registrations,
    /// The block this node is, registered with the flow that takes it in.
    block: Option<(Binding, Rc<BlockState>)>,
    /// The cell this node's own box is read into, made on first ask. Only a
    /// node that answers a hand is worth a cell, and one node stands in one
    /// box however many surfaces it carries, so the cell appears when the
    /// first of them asks and is shared by the rest.
    geometry: Option<Rc<Cell<MasonryRect>>>,
    boxes: Vec<NodeBox>,
    native: Vec<WidgetId>,
    window: Option<WindowTracker>,
}

impl<Action> MasonryNode<Action> {
    pub(crate) fn add_engine_control(
        &mut self,
        plan: HostedControlPlan<MasonryHostedState>,
        prepend: bool,
    ) {
        let node = self.widget.id();
        let Some(target) = EngineTarget::new(node, self.geometry(), plan) else {
            return;
        };
        let targets = &mut self.registrations.engine_targets;
        let index = if prepend { 0 } else { targets.len() };
        targets.insert(index, target.into());
    }

    pub(crate) fn add_popover(
        &mut self,
        layer: WidgetId,
        flag: &Binding,
        state: Rc<PopoverState>,
        dismiss: Rc<dyn Fn() -> HostAction>,
        held: Vec<WidgetId>,
    ) {
        let mut controls: Vec<WidgetId> = self
            .registrations
            .engines
            .iter()
            .map(|engine| engine.item.owner())
            .collect();
        controls.extend(held);
        self.registrations.popovers.push(
            PopoverRegistration {
                layer,
                state,
                dismiss,
                controls,
                anchor: self.widget.id(),
                flag: flag.clone(),
            }
            .into(),
        );
    }

    fn assemble(
        layout: NodeLayout,
        natural: Natural,
        children: Vec<Self>,
        background: Option<Rgba>,
        frame: Option<(FrameSides, Rgba, f32)>,
    ) -> Self {
        let mut child_widgets: Vec<WidgetPod<Node>> = Vec::with_capacity(children.len());
        let mut layers: Vec<NewWidget<dyn Widget>> = Vec::new();
        let mut registrations = Registrations::default();
        let mut boxes: Vec<NodeBox> = Vec::new();
        let mut native: Vec<WidgetId> = Vec::new();
        let mut window = None;
        let mut blocks: Vec<(Binding, Rc<BlockState>)> = Vec::new();
        for child in children {
            layers.extend(child.layers);
            registrations.extend(child.registrations);
            blocks.extend(child.block);
            boxes.extend(child.boxes);
            native.extend(child.native);
            window = merge_window(window, child.window);
            child_widgets.push(child.widget.to_pod());
        }
        let widget = NewWidget::new(Node::new(
            layout,
            natural.now(),
            child_widgets,
            background,
            frame,
        ));
        if widget.widget.is_native() {
            native.push(widget.id());
        }
        registrations.hides(widget.id(), blocks);
        Self {
            widget,
            natural,
            layers,
            registrations,
            boxes,
            native,
            window,
            action: PhantomData,
            block: None,
            geometry: None,
        }
    }

    /// The module shell's own bars, and whatever they hold.
    ///
    /// Chrome is furniture that holds furniture: it names no document path, so
    /// it is not announced as a document node, and neither is anything it holds.
    pub(crate) fn chrome(
        layout: NodeLayout,
        declared: solve::Size<solve::Length>,
        children: Vec<Self>,
        background: Option<Rgba>,
        frame: Option<(FrameSides, Rgba, f32)>,
    ) -> Self {
        Self::assemble(layout, declared.into(), children, background, frame)
    }

    /// A node standing for one node of the document.
    ///
    /// Announced as it is built, with whether the nodes it holds stand for
    /// document nodes of their own: a wrapper that only places, presses or
    /// scrolls its child speaks for the whole subtree it holds.
    pub(crate) fn document(
        layout: NodeLayout,
        natural: impl Into<Natural>,
        children: Vec<Self>,
        exposes_children: bool,
        background: Option<Rgba>,
        frame: Option<(FrameSides, Rgba, f32)>,
    ) -> Self {
        let node = Self::assemble(layout, natural.into(), children, background, frame);
        kithara::probe_event!(
            masonry_document_node,
            exposes_children,
            widget = node.widget.id().to_raw()
        );
        node
    }

    pub(crate) fn furniture(
        layout: NodeLayout,
        declared: solve::Size<solve::Length>,
        background: Option<Rgba>,
    ) -> Self {
        let widget = NewWidget::new(Node::new(layout, declared, Vec::new(), background, None));
        Self {
            widget,
            natural: Natural::Fixed(declared),
            action: PhantomData,
            layers: Vec::new(),
            registrations: Registrations::default(),
            block: None,
            geometry: None,
            boxes: Vec::new(),
            native: Vec::new(),
            window: None,
        }
    }

    /// The cell the root fills with the box this node stands in.
    pub(crate) fn geometry(&mut self) -> Rc<Cell<MasonryRect>> {
        if let Some(area) = &self.geometry {
            return Rc::clone(area);
        }
        let area = Rc::new(Cell::new(MasonryRect::ZERO));
        self.geometry = Some(Rc::clone(&area));
        self.boxes.push(NodeBox {
            area: Rc::clone(&area),
            node: self.widget.id(),
        });
        area
    }

    pub(crate) fn stage(size: Option<SizeSpec>, children: Vec<StageChild<Self>>) -> Self {
        let (sized, nodes) = children
            .into_iter()
            .map(|(block, floats, node)| ((block, floats, node.natural.clone()), node))
            .unzip();
        let size = Rc::new(StageSize::new(size, sized));
        Self::document(
            NodeLayout::Stage(Rc::clone(&size)),
            Natural::Stage(size),
            nodes,
            true,
            None,
            None,
        )
    }

    pub(crate) fn declared(&self) -> solve::Size<solve::Length> {
        self.natural.now()
    }

    pub(crate) fn host_engine(
        &mut self,
        map_event: Rc<dyn Fn(Published) -> HostAction>,
        skin: &Skin,
    ) {
        if self.registrations.engine_targets.is_empty() {
            return;
        }
        let targets = std::mem::take(&mut self.registrations.engine_targets);
        let raises_menu = targets
            .iter()
            .any(|target| matches!(target.item.plan, HostedControlPlan::Picker { .. }));
        let engine = HostedEngine::new(self.widget.id(), targets, map_event);
        if raises_menu {
            let layer = NewWidget::new(PickerLayer::new(Rc::clone(&engine), skin));
            engine.set_menu_layer(layer.id());
            self.layers.push(layer.erased());
        }
        self.registrations.engines.push(Rc::clone(&engine).into());
        self.widget.widget.set_engine(engine);
    }

    pub(crate) fn takes_drops(
        &mut self,
        path: String,
        frame: (FrameSides, Rgba, f32),
        pointer: Rc<Cell<Option<Pt>>>,
    ) {
        let node = &mut self.widget.widget;
        let idle = node.face();
        node.set_faces(Faces {
            idle,
            lit: Face {
                frame: Some(frame),
                ..idle
            },
        });
        let id = self.widget.id();
        self.registrations.watch(Watched::Zone { path, id });
        self.window = merge_window(
            self.window.take(),
            Some(WindowTracker {
                pointer,
                layer: None,
                carrying: false,
                drops: true,
            }),
        );
    }

    pub(crate) fn drops(&self) -> bool {
        self.window.as_ref().is_some_and(|window| window.drops)
    }

    /// Rounds the corners of this node's own box that the layout says are the
    /// window's own.
    ///
    /// The shape belongs to the node that paints the box, so it is set on the
    /// node after it is built rather than threaded through every constructor
    /// that never rounds anything.
    pub(crate) fn rounded(mut self, round: FrameCorners, radius: f32) -> Self {
        self.widget.widget.set_round(round, radius);
        self
    }

    pub(crate) fn set_window_layer(
        &mut self,
        pointer: Rc<Cell<Option<Pt>>>,
        layer: WidgetId,
        drops: bool,
    ) {
        self.window = Some(WindowTracker {
            pointer,
            carrying: false,
            drops,
            layer: Some(layer),
        });
    }

    pub(crate) fn set_window_pointer(&mut self, pointer: Rc<Cell<Option<Pt>>>) {
        match &mut self.window {
            Some(window) => window.pointer = pointer,
            None => {
                self.window = Some(WindowTracker {
                    pointer,
                    layer: None,
                    carrying: false,
                    drops: false,
                });
            }
        }
    }

    pub(crate) fn set_window_tracker(&mut self, tracker: WindowTracker) {
        self.window = Some(tracker);
    }

    pub(crate) fn widget_id(&self) -> WidgetId {
        self.widget.id()
    }

    /// Stands the mounted leaf in every enclosing object's pose. The pose is
    /// the node's Masonry transform, so what the leaf draws and the region
    /// that answers the pointer move as one, and a press lands in the leaf's
    /// own space through the inverse of the very transform it was painted
    /// with. A pose with no inverse is reported here: such a leaf still
    /// paints, but the node refuses every pointer it is offered.
    pub(crate) fn place(&mut self, path: &str, transform: Transform) {
        if !transform.is_invertible() {
            tracing::warn!(%path, "`{path}` will not take the pointer: its pose has no inverse");
        }
        self.widget.options.transform = affine(transform);
        self.widget.widget.place(transform);
    }

    delegate::delegate! {
        to self.widget.widget {
            /// What this node runs when it is pressed.
            pub(crate) fn set_primary(&mut self, primary: Box<dyn Fn() -> HostAction>);
            /// The stepping surface this flow carries over itself.
            pub(crate) fn set_detent(&mut self, detent: Detent);
            /// Where this placement of a stage stands, and what carries it.
            pub(crate) fn set_spot(&mut self, spot: Spot);
        }
        to self.layers {
            #[call(push)]
            pub(crate) fn add_layer(&mut self, layer: NewWidget<dyn Widget>);
            #[call(extend)]
            pub(crate) fn append_layers(&mut self, layers: Vec<NewWidget<dyn Widget>>);
        }
        to self.registrations {
            #[call(extend)]
            pub(crate) fn append_registrations(&mut self, registrations: Registrations);
        }
        to self.boxes {
            #[call(extend)]
            pub(crate) fn append_boxes(&mut self, boxes: Vec<NodeBox>);
        }
        to self.native {
            #[call(extend)]
            pub(crate) fn append_native(&mut self, native: Vec<WidgetId>);
        }
    }

    pub(crate) fn hidden_by(&mut self, hidden: Binding, state: &Rc<BlockState>) {
        self.registrations.stands_in(state);
        self.block = Some((hidden, Rc::clone(state)));
    }

    pub(crate) fn lights(&mut self, flag: Binding, faces: Option<Faces>) {
        if let Some(faces) = faces {
            self.widget.widget.set_faces(faces);
        }
        let id = self.widget.id();
        self.registrations.watch(Watched::Lit { flag, id });
    }

    pub(crate) fn watch(&mut self, binding: &Binding) {
        let id = self.widget.id();
        let binding = binding.clone();
        self.registrations.watch(Watched::Read { id, binding });
    }

    pub(crate) fn watch_custom(&mut self, path: &str) {
        let id = self.widget.id();
        let path = path.to_owned();
        self.registrations.watch(Watched::Custom { id, path });
    }

    pub(crate) fn watch_placement(&mut self, path: InternId) {
        let id = self.widget.id();
        self.registrations.watch(Watched::Placed { path, id });
    }

    pub(crate) fn watch_snapshot(&mut self) {
        let id = self.widget.id();
        self.registrations.watch(Watched::Snapshot { id });
    }

    pub(crate) fn watch_spot(&mut self, binding: &Binding) {
        let id = self.widget.id();
        let binding = binding.clone();
        self.registrations.watch(Watched::Spot { id, binding });
    }
}

impl<Action> From<MasonryNode<Action>> for LayerParts {
    fn from(node: MasonryNode<Action>) -> Self {
        (
            node.widget,
            node.natural.now(),
            node.layers,
            node.registrations,
            node.boxes,
            node.native,
            node.window,
        )
    }
}

impl<Action> From<MasonryNode<Action>> for RootParts {
    fn from(node: MasonryNode<Action>) -> Self {
        (
            node.widget.erased(),
            node.layers,
            node.registrations,
            node.boxes,
            node.native,
            node.window,
        )
    }
}

fn merge_window(
    left: Option<WindowTracker>,
    right: Option<WindowTracker>,
) -> Option<WindowTracker> {
    match (left, right) {
        (Some(window), Some(child)) => Some(WindowTracker {
            pointer: window.pointer,
            layer: window.layer.or(child.layer),
            carrying: window.carrying || child.carrying,
            drops: window.drops || child.drops,
        }),
        (Some(window), None) | (None, Some(window)) => Some(window),
        (None, None) => None,
    }
}

/// Stacks every modal above the other surfaces and layers, under only the
/// window's own layer, whatever its place in the document: the layer order is
/// the paint order, and the surface order is the order input finds them in.
pub(crate) fn stack_modals_on_top(
    layers: &mut [NewWidget<dyn Widget>],
    popovers: &mut [Within<PopoverRegistration>],
    window: Option<&WindowTracker>,
) {
    popovers.sort_by_key(|popover| matches!(popover.item.state.kind(), SurfaceKind::Modal));
    let modals: Vec<WidgetId> = popovers
        .iter()
        .filter(|popover| matches!(popover.item.state.kind(), SurfaceKind::Modal))
        .map(|popover| popover.item.layer)
        .collect();
    let edges = window.and_then(|window| window.layer);
    layers.sort_by_key(|layer| {
        let id = layer.id();
        (Some(id) == edges, modals.contains(&id))
    });
}

/// The widgets the application installed its own content in at `path`; the
/// branches of an adaptive block can mount one path more than once.
pub(crate) fn installed_at(watched: &[Within<Watched>], path: &str) -> Vec<WidgetId> {
    watched
        .iter()
        .filter_map(|watched| match &watched.item {
            Watched::Custom { id, path: at } if at == path => Some(*id),
            _ => None,
        })
        .collect()
}
