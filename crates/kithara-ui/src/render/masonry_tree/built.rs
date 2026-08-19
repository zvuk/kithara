use std::{cell::Cell, marker::PhantomData, rc::Rc};

use masonry::{
    core::{NewWidget, Widget, WidgetId},
    kurbo::Rect as MasonryRect,
};

use super::{
    custom::HostAction,
    menu::PickerLayer,
    mount::NodeLayout,
    node::Node,
    picker::{EngineTarget, HostedEngine},
    popover::PopoverState,
};
use crate::{
    draw::{Pt, Rgba, Transform},
    expand::Binding,
    ids::InternId,
    layout::FrameSides,
    render::{HostedControlPlan, Skin, UiEvent},
    solve,
};

type PopoverRegistration = (WidgetId, Rc<PopoverState>, Rc<dyn Fn() -> HostAction>);
type WindowTracker = (Rc<Cell<Option<Pt>>>, Option<WidgetId>, bool);
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
}
pub(super) type LayerParts = (
    NewWidget<Node>,
    solve::Size<solve::Length>,
    Vec<NewWidget<dyn Widget>>,
    Vec<PopoverRegistration>,
    Vec<EngineTarget>,
    Vec<Rc<HostedEngine>>,
    Vec<WidgetId>,
    Option<WindowTracker>,
    Vec<Watched>,
);
pub(super) type RootParts = (
    NewWidget<dyn Widget>,
    Vec<NewWidget<dyn Widget>>,
    Vec<PopoverRegistration>,
    Vec<Rc<HostedEngine>>,
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
    engine_targets: Vec<EngineTarget>,
    engines: Vec<Rc<HostedEngine>>,
    watched: Vec<Watched>,
    native: Vec<WidgetId>,
    window: Option<WindowTracker>,
    #[cfg(test)]
    document_ids: Vec<WidgetId>,
}

impl<Action> MasonryNode<Action> {
    pub(super) fn document(
        layout: NodeLayout,
        declared: solve::Size<solve::Length>,
        children: Vec<Self>,
        _expose_children: bool,
        background: Option<Rgba>,
        frame: Option<(FrameSides, Rgba, f32)>,
    ) -> Self {
        let mut child_widgets = Vec::with_capacity(children.len());
        let mut layers = Vec::new();
        let mut popovers = Vec::new();
        let mut engine_targets = Vec::new();
        let mut engines = Vec::new();
        let mut watched = Vec::new();
        let mut native = Vec::new();
        let mut window = None;
        #[cfg(test)]
        let mut child_ids = Vec::new();
        for child in children {
            layers.extend(child.layers);
            popovers.extend(child.popovers);
            engine_targets.extend(child.engine_targets);
            engines.extend(child.engines);
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
            let mut ids = Vec::with_capacity(child_ids.len() + 1);
            ids.push(widget.id());
            ids.extend(child_ids);
            ids
        };
        Self {
            widget,
            declared,
            action: PhantomData,
            layers,
            popovers,
            engine_targets,
            engines,
            watched,
            native,
            window,
            #[cfg(test)]
            document_ids,
        }
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
            engine_targets: Vec::new(),
            engines: Vec::new(),
            watched: Vec::new(),
            native: Vec::new(),
            window: None,
            #[cfg(test)]
            document_ids: Vec::new(),
        }
    }

    delegate::delegate! {
        to self.widget.widget {
            /// What this node runs when pressed, and with the other button.
            pub(crate) fn set_actions(
                &mut self,
                primary: Option<Box<dyn Fn() -> HostAction>>,
                secondary: Option<Box<dyn Fn() -> HostAction>>,
            );
            /// The cell this node will publish its laid-out box into.
            pub(crate) fn geometry(&mut self) -> Rc<Cell<MasonryRect>>;
            /// Offsets everything the mounted leaf draws, without moving the
            /// box the layout gave it or the region that answers the pointer.
            /// Nothing is standing yet at mount, so the answer is discarded.
            pub(crate) fn place(&mut self, transform: Transform) -> bool;
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
        to self.engine_targets {
            #[call(extend)]
            pub(crate) fn append_engine_targets(&mut self, targets: Vec<EngineTarget>);
        }
        to self.engines {
            #[call(extend)]
            pub(crate) fn append_engines(&mut self, engines: Vec<Rc<HostedEngine>>);
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

    pub(crate) fn add_popover(
        &mut self,
        state: Rc<PopoverState>,
        dismiss: Rc<dyn Fn() -> HostAction>,
    ) {
        self.popovers.push((self.widget.id(), state, dismiss));
    }

    pub(crate) fn add_engine_control(&mut self, plan: HostedControlPlan, prepend: bool) {
        let Some(target) = EngineTarget::new(self.geometry(), plan) else {
            return;
        };
        let index = if prepend {
            0
        } else {
            self.engine_targets.len()
        };
        self.engine_targets.insert(index, target);
    }

    /// Remembers that this node's leaf shows one endpoint, so its value can be
    /// re-read into the mounted tree instead of rebuilding the tree to show it.
    pub(crate) fn watch(&mut self, binding: &Binding) {
        self.watched.push(Watched::Read {
            id: self.widget.id(),
            binding: binding.clone(),
        });
    }

    pub(crate) fn watch_snapshot(&mut self) {
        self.watched.push(Watched::Snapshot {
            id: self.widget.id(),
        });
    }

    /// Remembers that an object places this node, so the document walk can put
    /// it somewhere else without the tree being rebuilt around it.
    pub(crate) fn watch_placement(&mut self, path: InternId) {
        self.watched.push(Watched::Placed {
            id: self.widget.id(),
            path,
        });
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
        // The menu hangs outside every box in the tree, so it is drawn by a
        // layer above it rather than by the control that owns the engine.
        if raises_menu {
            let layer = NewWidget::new(PickerLayer::new(Rc::clone(&engine), skin));
            engine.set_menu_layer(layer.id());
            self.layers.push(layer.erased());
        }
        self.engines.push(Rc::clone(&engine));
        self.widget.widget.set_engine(engine);
    }

    pub(crate) fn set_window_pointer(&mut self, pointer: Rc<Cell<Option<Pt>>>) {
        let (layer, repaint) = self
            .window
            .as_ref()
            .map_or((None, false), |(_, layer, repaint)| (*layer, *repaint));
        self.window = Some((pointer, layer, repaint));
    }

    pub(crate) fn set_window_layer(
        &mut self,
        pointer: Rc<Cell<Option<Pt>>>,
        layer: WidgetId,
        repaint: bool,
    ) {
        self.window = Some((pointer, Some(layer), repaint));
    }

    pub(crate) fn set_window_tracker(&mut self, tracker: WindowTracker) {
        self.window = Some(tracker);
    }

    #[cfg(test)]
    pub(crate) fn document_ids(&self) -> &[WidgetId] {
        &self.document_ids
    }

    #[cfg(test)]
    pub(crate) fn widget_id(&self) -> WidgetId {
        self.widget.id()
    }
}

impl<Action> From<MasonryNode<Action>> for LayerParts {
    fn from(node: MasonryNode<Action>) -> Self {
        (
            node.widget,
            node.declared,
            node.layers,
            node.popovers,
            node.engine_targets,
            node.engines,
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
            node.engines,
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
        (Some((pointer, layer, repaint)), Some((_, child_layer, child_repaint))) => {
            Some((pointer, layer.or(child_layer), repaint || child_repaint))
        }
        (Some(window), None) | (None, Some(window)) => Some(window),
        (None, None) => None,
    }
}
