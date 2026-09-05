use std::{cell::Cell, marker::PhantomData, rc::Rc};

use masonry::{
    core::{NewWidget, Widget, WidgetId, WidgetPod},
    kurbo::Rect as MasonryRect,
};

use super::{
    custom::HostAction,
    menu::PickerLayer,
    mount::NodeLayout,
    node::{Detent, Faces, Node},
    picker::{EngineTarget, HostedEngine},
    popover::PopoverState,
    spot::Spot,
};
use crate::{
    draw::{Pt, Rgba, Transform},
    expand::Binding,
    ids::InternId,
    layout::{FrameCorners, FrameSides},
    render::{HostedControlPlan, Skin, UiEvent},
    solve,
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
    /// The node the surface opens from.
    pub(crate) anchor: WidgetId,
    /// The layer the surface is drawn in.
    pub(crate) layer: WidgetId,
    /// The engine-driven controls the open surface answers for: the anchor it
    /// opens from, and everything the surface itself holds.
    ///
    /// An engine answers the pointer against its own box and cannot see what
    /// stands above it, while the document lays out siblings the open surface
    /// hangs across. So the surface names the controls it covers the room for.
    /// The anchor is one of them because a surface is closed by the control
    /// that opened it, and a surface wide enough covers that control too.
    pub(crate) controls: Vec<WidgetId>,
}

/// The window layer one tree mounted, and what a root needs to keep it in step
/// with the document it came from.
pub(crate) struct WindowTracker {
    /// What the pointer carries, re-read whenever the document is shown again.
    pub(crate) carried: Option<Binding>,
    pub(crate) layer: Option<WidgetId>,
    pub(crate) pointer: Rc<Cell<Option<Pt>>>,
    /// Whether the last reading found anything, which is when the layer has to
    /// be painted again as the pointer moves.
    pub(crate) carrying: bool,
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

pub(super) type LayerParts = (
    NewWidget<Node>,
    solve::Size<solve::Length>,
    Vec<NewWidget<dyn Widget>>,
    Vec<PopoverRegistration>,
    Vec<BlockRegistration>,
    Vec<EngineTarget>,
    Vec<Rc<HostedEngine>>,
    Vec<NodeBox>,
    Vec<WidgetId>,
    Option<WindowTracker>,
    Vec<Watched>,
);
pub(super) type RootParts = (
    NewWidget<dyn Widget>,
    Vec<NewWidget<dyn Widget>>,
    Vec<PopoverRegistration>,
    Vec<BlockRegistration>,
    Vec<Rc<HostedEngine>>,
    Vec<NodeBox>,
    Vec<WidgetId>,
    Option<WindowTracker>,
    Vec<Watched>,
);

/// A retained Masonry tree produced by the document facade.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct MasonryNode<Action> {
    widget: NewWidget<Node>,
    #[field(get(copy), vis = "pub(crate)")]
    declared: solve::Size<solve::Length>,
    action: PhantomData<fn() -> Action>,
    layers: Vec<NewWidget<dyn Widget>>,
    popovers: Vec<PopoverRegistration>,
    blocks: Vec<BlockRegistration>,
    engine_targets: Vec<EngineTarget>,
    engines: Vec<Rc<HostedEngine>>,
    /// The cell this node's own box is read into, made on first ask. Only a
    /// node that answers a hand is worth a cell, and one node stands in one
    /// box however many surfaces it carries, so the cell appears when the
    /// first of them asks and is shared by the rest.
    geometry: Option<Rc<Cell<MasonryRect>>>,
    boxes: Vec<NodeBox>,
    watched: Vec<Watched>,
    native: Vec<WidgetId>,
    window: Option<WindowTracker>,
    #[cfg(test)]
    document_ids: Vec<WidgetId>,
}

impl<Action> MasonryNode<Action> {
    pub(crate) fn add_engine_control(&mut self, plan: HostedControlPlan, prepend: bool) {
        let node = self.widget.id();
        let Some(target) = EngineTarget::new(node, self.geometry(), plan) else {
            return;
        };
        let index = if prepend {
            0
        } else {
            self.engine_targets.len()
        };
        self.engine_targets.insert(index, target);
    }

    pub(crate) fn add_popover(
        &mut self,
        layer: WidgetId,
        flag: &Binding,
        state: Rc<PopoverState>,
        dismiss: Rc<dyn Fn() -> HostAction>,
        held: Vec<WidgetId>,
    ) {
        let mut controls: Vec<WidgetId> =
            self.engines.iter().map(|engine| engine.owner()).collect();
        controls.extend(held);
        self.popovers.push(PopoverRegistration {
            layer,
            state,
            dismiss,
            controls,
            anchor: self.widget.id(),
            flag: flag.clone(),
        });
    }

    /// The module shell's own bars, and whatever they hold.
    ///
    /// Chrome is furniture that holds furniture: it names no document path, so
    /// it must not answer as a document node when a test counts them.
    pub(super) fn chrome(
        layout: NodeLayout,
        declared: solve::Size<solve::Length>,
        children: Vec<Self>,
        background: Option<Rgba>,
        frame: Option<(FrameSides, Rgba, f32)>,
    ) -> Self {
        let node = Self::document(layout, declared, children, false, background, frame);
        #[cfg(test)]
        let node = Self {
            document_ids: Vec::new(),
            ..node
        };
        node
    }

    pub(super) fn document(
        layout: NodeLayout,
        declared: solve::Size<solve::Length>,
        children: Vec<Self>,
        _expose_children: bool,
        background: Option<Rgba>,
        frame: Option<(FrameSides, Rgba, f32)>,
    ) -> Self {
        let mut child_widgets: Vec<WidgetPod<Node>> = Vec::with_capacity(children.len());
        let mut layers: Vec<NewWidget<dyn Widget>> = Vec::new();
        let mut popovers: Vec<PopoverRegistration> = Vec::new();
        let mut blocks: Vec<BlockRegistration> = Vec::new();
        let mut engine_targets: Vec<EngineTarget> = Vec::new();
        let mut engines: Vec<Rc<HostedEngine>> = Vec::new();
        let mut boxes: Vec<NodeBox> = Vec::new();
        let mut watched: Vec<Watched> = Vec::new();
        let mut native: Vec<WidgetId> = Vec::new();
        let mut window = None;
        #[cfg(test)]
        let mut child_ids: Vec<WidgetId> = Vec::new();
        for child in children {
            layers.extend(child.layers);
            popovers.extend(child.popovers);
            blocks.extend(child.blocks);
            engine_targets.extend(child.engine_targets);
            engines.extend(child.engines);
            boxes.extend(child.boxes);
            watched.extend(child.watched);
            native.extend(child.native);
            window = merge_window(window, child.window);
            #[cfg(test)]
            if _expose_children {
                child_ids.extend(child.document_ids);
            }
            child_widgets.push(child.widget.to_pod());
        }
        let widget = NewWidget::new(Node::new(
            layout,
            declared,
            child_widgets,
            background,
            frame,
        ));
        if widget.widget.is_native() {
            native.push(widget.id());
        }
        #[cfg(test)]
        let document_ids = {
            let mut ids: Vec<WidgetId> = Vec::with_capacity(child_ids.len() + 1);
            ids.push(widget.id());
            ids.extend(child_ids);
            ids
        };
        Self {
            widget,
            declared,
            layers,
            popovers,
            blocks,
            engine_targets,
            engines,
            boxes,
            watched,
            native,
            window,
            #[cfg(test)]
            document_ids,
            action: PhantomData,
            geometry: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn document_ids(&self) -> &[WidgetId] {
        &self.document_ids
    }

    pub(super) fn furniture(
        layout: NodeLayout,
        declared: solve::Size<solve::Length>,
        background: Option<Rgba>,
    ) -> Self {
        let widget = NewWidget::new(Node::new(layout, declared, Vec::new(), background, None));
        Self {
            widget,
            declared,
            action: PhantomData,
            layers: Vec::new(),
            popovers: Vec::new(),
            blocks: Vec::new(),
            engine_targets: Vec::new(),
            engines: Vec::new(),
            geometry: None,
            boxes: Vec::new(),
            watched: Vec::new(),
            native: Vec::new(),
            window: None,
            #[cfg(test)]
            document_ids: Vec::new(),
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

    /// Remembers the flag this node is dressed by, and the two faces it chooses
    /// between where the faces are the node's own rather than its leaf's, so the
    /// root can read the flag again without building the tree afresh.
    pub(crate) fn lights(&mut self, flag: Binding, faces: Option<Faces>) {
        if let Some(faces) = faces {
            self.widget.widget.set_faces(faces);
        }
        self.watched.push(Watched::Lit {
            id: self.widget.id(),
            flag,
        });
    }

    /// Remembers that this node hides the blocks among its own children, so
    /// the root can read them again and lay this node out when one changes.
    pub(crate) fn hides(&mut self, blocks: Vec<(Binding, Rc<BlockState>)>) {
        let flow = self.widget.id();
        self.blocks
            .extend(blocks.into_iter().map(|(hidden, state)| BlockRegistration {
                hidden,
                state,
                flow,
            }));
    }

    pub(crate) fn host_engine(
        &mut self,
        map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
        skin: &Skin,
    ) {
        if self.engine_targets.is_empty() {
            return;
        }
        let targets = std::mem::take(&mut self.engine_targets);
        let raises_menu = targets
            .iter()
            .any(|target| matches!(target.plan, HostedControlPlan::Picker { .. }));
        let engine = HostedEngine::new(self.widget.id(), targets, map_event);
        if raises_menu {
            let layer = NewWidget::new(PickerLayer::new(Rc::clone(&engine), skin));
            engine.set_menu_layer(layer.id());
            self.layers.push(layer.erased());
        }
        self.engines.push(Rc::clone(&engine));
        self.widget.widget.set_engine(engine);
    }

    /// Rounds the corners of this node's own box that the layout says are the
    /// window's own.
    ///
    /// The shape belongs to the node that paints the box, so it is set on the
    /// node after it is built rather than threaded through every constructor
    /// that never rounds anything.
    pub(super) fn rounded(mut self, round: FrameCorners, radius: f32) -> Self {
        self.widget.widget.set_round(round, radius);
        self
    }

    pub(crate) fn set_window_layer(
        &mut self,
        pointer: Rc<Cell<Option<Pt>>>,
        layer: WidgetId,
        carried: Option<Binding>,
        carrying: bool,
    ) {
        self.window = Some(WindowTracker {
            pointer,
            carried,
            carrying,
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
                    carried: None,
                    carrying: false,
                });
            }
        }
    }

    pub(crate) fn set_window_tracker(&mut self, tracker: WindowTracker) {
        self.window = Some(tracker);
    }

    /// Remembers that this node's leaf shows one endpoint, so its value can be
    /// re-read into the mounted tree instead of rebuilding the tree to show it.
    pub(crate) fn watch(&mut self, binding: &Binding) {
        self.watched.push(Watched::Read {
            id: self.widget.id(),
            binding: binding.clone(),
        });
    }

    /// Remembers that an object places this node, so the document walk can put
    /// it somewhere else without the tree being rebuilt around it.
    pub(crate) fn watch_placement(&mut self, path: InternId) {
        self.watched.push(Watched::Placed {
            path,
            id: self.widget.id(),
        });
    }

    pub(crate) fn watch_snapshot(&mut self) {
        self.watched.push(Watched::Snapshot {
            id: self.widget.id(),
        });
    }

    /// Remembers that this placement reads its point from an endpoint, so the
    /// stage lays it out again where the point moves.
    pub(crate) fn watch_spot(&mut self, binding: &Binding) {
        self.watched.push(Watched::Spot {
            id: self.widget.id(),
            binding: binding.clone(),
        });
    }

    #[cfg(any(test, feature = "capture"))]
    pub(crate) fn widget_id(&self) -> WidgetId {
        self.widget.id()
    }

    delegate::delegate! {
        to self.widget.widget {
            /// What this node runs when pressed, and with the other button.
            pub(crate) fn set_actions(
                &mut self,
                primary: Option<Box<dyn Fn() -> HostAction>>,
                secondary: Option<Box<dyn Fn() -> HostAction>>,
            );
            /// Offsets everything the mounted leaf draws, without moving the
            /// box the layout gave it or the region that answers the pointer.
            /// Nothing is standing yet at mount, so the answer is discarded.
            pub(crate) fn place(&mut self, transform: Transform) -> bool;
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
        to self.popovers {
            #[call(extend)]
            pub(crate) fn append_popovers(&mut self, popovers: Vec<PopoverRegistration>);
        }
        to self.blocks {
            #[call(extend)]
            pub(crate) fn append_blocks(&mut self, blocks: Vec<BlockRegistration>);
        }
        to self.engine_targets {
            #[call(extend)]
            pub(crate) fn append_engine_targets(&mut self, targets: Vec<EngineTarget>);
        }
        to self.engines {
            #[call(extend)]
            pub(crate) fn append_engines(&mut self, engines: Vec<Rc<HostedEngine>>);
        }
        to self.boxes {
            #[call(extend)]
            pub(crate) fn append_boxes(&mut self, boxes: Vec<NodeBox>);
        }
        to self.watched {
            #[call(extend)]
            pub(crate) fn append_watched(&mut self, watched: Vec<Watched>);
        }
        to self.native {
            #[call(extend)]
            pub(crate) fn append_native(&mut self, native: Vec<WidgetId>);
        }
    }
}

impl<Action> From<MasonryNode<Action>> for LayerParts {
    fn from(node: MasonryNode<Action>) -> Self {
        (
            node.widget,
            node.declared,
            node.layers,
            node.popovers,
            node.blocks,
            node.engine_targets,
            node.engines,
            node.boxes,
            node.native,
            node.window,
            node.watched,
        )
    }
}

impl<Action> From<MasonryNode<Action>> for RootParts {
    fn from(node: MasonryNode<Action>) -> Self {
        (
            node.widget.erased(),
            node.layers,
            node.popovers,
            node.blocks,
            node.engines,
            node.boxes,
            node.native,
            node.window,
            node.watched,
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
            carried: window.carried.or(child.carried),
            carrying: window.carrying || child.carrying,
        }),
        (Some(window), None) | (None, Some(window)) => Some(window),
        (None, None) => None,
    }
}
