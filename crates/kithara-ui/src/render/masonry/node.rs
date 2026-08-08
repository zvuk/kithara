use std::{cell::Cell, marker::PhantomData, rc::Rc};

use masonry::{
    accesskit::{Node as AccessNode, Role},
    core::{
        AccessCtx, AllowRawMut, BoxConstraints, ChildrenIds, ComposeCtx, CursorIcon, EventCtx,
        LayoutCtx, NewWidget, PaintCtx, PointerButton as MasonryPointerButton, PointerEvent,
        PropertiesMut, PropertiesRef, QueryCtx, RegisterCtx, TextEvent, Update, UpdateCtx, Widget,
        WidgetId, WidgetPod,
    },
    kurbo::{Affine, Point, Rect as MasonryRect, Size as MasonrySize},
    vello::Scene,
};
use num_traits::cast::AsPrimitive;
use tracing::{Span, trace_span};

use super::{
    Repaint,
    custom::HostAction,
    leaf::cursor_icon,
    mount::NodeLayout,
    picker::{EngineTarget, HostedEngine},
    popover::PopoverState,
};
use crate::{
    backends::VelloBackend,
    draw::{DrawListBuilder, Pt, Rect, Rgba, replay},
    expand::Binding,
    interact::{
        CursorShape, Hit, Input, MOUSE, PointerButton, PointerInput, PointerOwnership,
        PointerPhase,
        masonry::{portable_modifiers, portable_text_input},
    },
    layout::FrameSides,
    render::{HostedControlPlan, ReadValue, UiEvent},
    solve,
};

type PopoverRegistration = (WidgetId, Rc<PopoverState>, Rc<dyn Fn() -> HostAction>);
type WindowTracker = (Rc<Cell<Option<Pt>>>, Option<WidgetId>, bool);
/// One mounted leaf and the endpoint it shows, so the value can be re-read into
/// it without the tree being rebuilt around it.
pub(crate) type Watched = (WidgetId, Binding);
pub(super) type LayerParts = (
    NewWidget<Node>,
    solve::Size<solve::Length>,
    Vec<NewWidget<dyn Widget>>,
    Vec<PopoverRegistration>,
    Vec<EngineTarget>,
    Vec<Rc<HostedEngine>>,
    Option<WindowTracker>,
    Vec<Watched>,
);
pub(super) type RootParts = (
    NewWidget<dyn Widget>,
    Vec<NewWidget<dyn Widget>>,
    Vec<PopoverRegistration>,
    Vec<Rc<HostedEngine>>,
    Option<WindowTracker>,
    Vec<Watched>,
);

#[derive(Clone, Copy, Eq, PartialEq)]
enum NodePointerOwner {
    Leaf,
    Engine,
}

pub(crate) struct Node {
    layout: NodeLayout,
    declared: solve::Size<solve::Length>,
    limits: Option<solve::Limits>,
    children: Vec<WidgetPod<Self>>,
    primary: Option<Box<dyn Fn() -> HostAction>>,
    secondary: Option<Box<dyn Fn() -> HostAction>>,
    background: Option<Rgba>,
    frame: Option<(FrameSides, Rgba, f32)>,
    double_click: bool,
    pointer: Option<(Pt, Pt)>,
    pointer_owner: Option<NodePointerOwner>,
    geometry: Option<Rc<Cell<MasonryRect>>>,
    engine: Option<Rc<HostedEngine>>,
}

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
    window: Option<WindowTracker>,
    #[cfg(test)]
    document_ids: Vec<WidgetId>,
}

impl Node {
    fn new(
        layout: NodeLayout,
        declared: solve::Size<solve::Length>,
        children: Vec<WidgetPod<Self>>,
        background: Option<Rgba>,
        frame: Option<(FrameSides, Rgba, f32)>,
    ) -> Self {
        Self {
            layout,
            declared,
            limits: None,
            children,
            primary: None,
            secondary: None,
            background,
            frame,
            double_click: false,
            pointer: None,
            pointer_owner: None,
            geometry: None,
            engine: None,
        }
    }

    pub(crate) fn set_child_limits(
        ctx: &mut LayoutCtx<'_>,
        child: &mut WidgetPod<Self>,
        limits: solve::Limits,
    ) {
        let (node, mut raw) = ctx.get_raw_mut(child);
        if node.limits != Some(limits) {
            node.limits = Some(limits);
            raw.request_layout();
        }
    }

    fn queue_pointer(&self, ctx: &mut EventCtx<'_>, event: &PointerEvent) {
        let PointerEvent::Down(button) = event else {
            return;
        };
        let action = match button.button {
            Some(MasonryPointerButton::Secondary) => self.secondary.as_ref(),
            Some(MasonryPointerButton::Primary) | None => self.primary.as_ref(),
            _ => None,
        };
        if let Some(action) = action {
            ctx.submit_action::<HostAction>(action());
            ctx.set_handled();
        }
    }

    fn leaf_input(
        &mut self,
        ctx: &mut EventCtx<'_>,
        event: &PointerEvent,
        input: Input<'_>,
        hit: Hit,
    ) -> bool {
        let Some(leaf) = self.layout.leaf() else {
            return false;
        };
        let retained = self.pointer_owner == Some(NodePointerOwner::Leaf);
        let outcome = leaf.input(input, hit);
        if !matches!(leaf.repaint(), Repaint::None) {
            ctx.request_anim_frame();
            ctx.request_paint_only();
        }
        if matches!(event, PointerEvent::Down(_)) && hit.over() {
            ctx.request_focus();
        }
        let captured = outcome.is_captured();
        match outcome.ownership() {
            PointerOwnership::Claim if matches!(event, PointerEvent::Down(_)) => {
                ctx.capture_pointer();
                self.pointer_owner = Some(NodePointerOwner::Leaf);
            }
            PointerOwnership::Release if retained => {
                ctx.release_pointer();
                self.pointer_owner = None;
            }
            PointerOwnership::Unchanged | PointerOwnership::Claim | PointerOwnership::Release => {}
        }
        if let Some(action) = outcome.value() {
            ctx.submit_action::<HostAction>(action);
        }
        let exclusive = captured || retained;
        if retained
            && self.pointer_owner.is_some()
            && matches!(event, PointerEvent::Up(_) | PointerEvent::Cancel(_))
        {
            ctx.release_pointer();
            self.pointer_owner = None;
        }
        if exclusive {
            ctx.set_handled();
        }
        exclusive
    }

    /// Shows the value an engine just published for this node's control, without
    /// waiting for the rebuild that follows the gesture.
    pub(crate) fn show_live(&mut self, value: &ReadValue<'_>) -> bool {
        self.layout.leaf().is_some_and(|leaf| leaf.set_read(value))
    }

    fn engine_input(
        &mut self,
        ctx: &mut EventCtx<'_>,
        event: &PointerEvent,
        input: Input<'_>,
        point: Option<Pt>,
    ) -> bool {
        let Some(engine) = self.engine.as_ref().map(Rc::clone) else {
            return false;
        };
        let retained = self.pointer_owner == Some(NodePointerOwner::Engine);
        let routed = engine.route(input, point);
        let (outcome, focused) = (routed.outcome, routed.focused);
        if matches!(event, PointerEvent::Down(_)) {
            if focused {
                ctx.request_focus();
            } else if ctx.has_focus_target() {
                ctx.resign_focus();
            }
        }
        sync_ime_area(ctx, &engine);
        let captured = outcome.is_captured();
        match outcome.ownership() {
            PointerOwnership::Claim if matches!(event, PointerEvent::Down(_)) => {
                ctx.capture_pointer();
                self.pointer_owner = Some(NodePointerOwner::Engine);
            }
            PointerOwnership::Release if retained => {
                ctx.release_pointer();
                self.pointer_owner = None;
            }
            PointerOwnership::Unchanged | PointerOwnership::Claim | PointerOwnership::Release => {}
        }
        if let Some(action) = outcome.value() {
            ctx.submit_action::<HostAction>(action);
        }
        let exclusive = captured || retained;
        if retained
            && self.pointer_owner.is_some()
            && matches!(event, PointerEvent::Up(_) | PointerEvent::Cancel(_))
        {
            ctx.release_pointer();
            self.pointer_owner = None;
        }
        if exclusive {
            ctx.set_handled();
        }
        exclusive
    }

    fn leaf_text_input(&mut self, ctx: &mut EventCtx<'_>, input: Input<'_>) -> bool {
        let Some(leaf) = self.layout.leaf() else {
            return false;
        };
        let size = ctx.size();
        let outcome = leaf.input(
            input,
            Hit::new(
                None,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: size.width.as_(),
                    h: size.height.as_(),
                },
            ),
        );
        if !matches!(leaf.repaint(), Repaint::None) {
            ctx.request_anim_frame();
            ctx.request_paint_only();
        }
        let captured = outcome.is_captured();
        if let Some(action) = outcome.value() {
            ctx.submit_action::<HostAction>(action);
        }
        if captured {
            ctx.set_handled();
        }
        captured
    }

    fn engine_text_input(&self, ctx: &mut EventCtx<'_>, input: Input<'_>) -> bool {
        let Some(engine) = &self.engine else {
            return false;
        };
        let outcome = engine.route(input, None).outcome;
        sync_ime_area(ctx, engine);
        let captured = outcome.is_captured();
        if let Some(action) = outcome.value() {
            ctx.submit_action::<HostAction>(action);
        }
        if captured {
            ctx.request_paint_only();
            ctx.set_handled();
        }
        captured
    }

    fn route_text_input(&mut self, ctx: &mut EventCtx<'_>, input: Input<'_>) {
        if self.leaf_text_input(ctx, input) {
            return;
        }
        self.engine_text_input(ctx, input);
    }

    fn pointer_input(
        &mut self,
        ctx: &EventCtx<'_>,
        event: &PointerEvent,
    ) -> Option<(Input<'static>, Hit, Option<Pt>)> {
        let position = match event {
            PointerEvent::Down(button) | PointerEvent::Up(button) => Some(button.state.position),
            PointerEvent::Move(update) => Some(update.current.position),
            PointerEvent::Scroll(scroll) => Some(scroll.state.position),
            PointerEvent::Gesture(gesture) => Some(gesture.state.position),
            PointerEvent::Cancel(_) | PointerEvent::Enter(_) | PointerEvent::Leave(_) => None,
        };
        if let Some(position) = position {
            let scale = ctx.get_scale_factor();
            let local = ctx.local_position(position);
            self.pointer = Some((
                Pt {
                    x: (position.x / scale).as_(),
                    y: (position.y / scale).as_(),
                },
                Pt {
                    x: local.x.as_(),
                    y: local.y.as_(),
                },
            ));
        }
        let pointer = match event {
            PointerEvent::Down(button) => {
                self.double_click = button.state.count >= 2;
                Some((
                    PointerPhase::Down,
                    button.button.map(pointer_button),
                    button.state.count,
                ))
            }
            PointerEvent::Move(update) => Some((PointerPhase::Move, None, update.current.count)),
            PointerEvent::Up(button) => {
                let phase = if std::mem::take(&mut self.double_click) {
                    PointerPhase::DoubleClick
                } else {
                    PointerPhase::Up
                };
                Some((phase, button.button.map(pointer_button), button.state.count))
            }
            PointerEvent::Leave(_) => Some((PointerPhase::Leave, None, 0)),
            PointerEvent::Cancel(_) => Some((PointerPhase::Cancel, None, 0)),
            PointerEvent::Enter(_) | PointerEvent::Scroll(_) | PointerEvent::Gesture(_) => None,
        };
        let (window, local) = if matches!(event, PointerEvent::Leave(_) | PointerEvent::Cancel(_)) {
            (None, None)
        } else {
            self.pointer.unzip()
        };
        let size = ctx.size();
        let hit = Hit::new(
            local,
            Rect {
                x: 0.0,
                y: 0.0,
                w: size.width.as_(),
                h: size.height.as_(),
            },
        );
        let input = match (pointer, event) {
            (Some((phase, button, clicks)), _) => {
                Input::Pointer(PointerInput::new(MOUSE, button, phase, window, clicks))
            }
            (None, PointerEvent::Scroll(scroll)) => {
                let scale = ctx.get_scale_factor();
                let scroll = match scroll.delta {
                    masonry::ui_events::ScrollDelta::LineDelta(x, y) => {
                        crate::interact::Scroll::Lines { x, y }
                    }
                    masonry::ui_events::ScrollDelta::PixelDelta(delta) => {
                        crate::interact::Scroll::Pixels {
                            x: (delta.x / scale).as_(),
                            y: (delta.y / scale).as_(),
                        }
                    }
                    masonry::ui_events::ScrollDelta::PageDelta(_, _) => return None,
                };
                Input::Wheel(scroll)
            }
            (None, _) => return None,
        };
        Some((input, hit, window))
    }

    fn paint_surface(&self, bounds: Rect, list: &mut DrawListBuilder) {
        if let Some(color) = self.background {
            list.fill_rect(bounds, color);
        }
        let Some((sides, color, width)) = self.frame else {
            return;
        };
        let right = bounds.x + bounds.w;
        let bottom = bounds.y + bounds.h;
        if sides.top {
            list.stroke_line(
                Pt {
                    x: bounds.x,
                    y: bounds.y,
                },
                Pt {
                    x: right,
                    y: bounds.y,
                },
                color,
                width,
            );
        }
        if sides.right {
            list.stroke_line(
                Pt {
                    x: right,
                    y: bounds.y,
                },
                Pt {
                    x: right,
                    y: bottom,
                },
                color,
                width,
            );
        }
        if sides.bottom {
            list.stroke_line(
                Pt {
                    x: bounds.x,
                    y: bottom,
                },
                Pt {
                    x: right,
                    y: bottom,
                },
                color,
                width,
            );
        }
        if sides.left {
            list.stroke_line(
                Pt {
                    x: bounds.x,
                    y: bounds.y,
                },
                Pt {
                    x: bounds.x,
                    y: bottom,
                },
                color,
                width,
            );
        }
    }
}

impl AllowRawMut for Node {}

impl Widget for Node {
    type Action = HostAction;

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        let Some((input, hit, point)) = self.pointer_input(ctx, event) else {
            self.queue_pointer(ctx, event);
            return;
        };
        match self.pointer_owner {
            Some(NodePointerOwner::Leaf) => {
                self.leaf_input(ctx, event, input, hit);
                return;
            }
            Some(NodePointerOwner::Engine) => {
                self.engine_input(ctx, event, input, point);
                return;
            }
            None => {}
        }
        if self.leaf_input(ctx, event, input, hit) {
            return;
        }
        if self.pointer_owner.is_some() {
            return;
        }
        if self.engine_input(ctx, event, input, point) {
            return;
        }
        if self.pointer_owner.is_some() {
            return;
        }
        self.queue_pointer(ctx, event);
    }

    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if let TextEvent::Keyboard(event) = event {
            self.route_text_input(
                ctx,
                Input::ModifiersChanged(portable_modifiers(event.modifiers)),
            );
        }
        if let Some(input) = portable_text_input(event) {
            self.route_text_input(ctx, input);
        }
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        for child in &mut self.children {
            ctx.register_child(child);
        }
    }

    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _props: &mut PropertiesMut<'_>, event: &Update) {
        if matches!(event, Update::WidgetAdded)
            && let Some(leaf) = self.layout.leaf()
        {
            leaf.added(ctx);
        }
        if let Update::HoveredChanged(hovered) = event
            && let Some(leaf) = self.layout.leaf()
            && leaf.hover(*hovered)
        {
            ctx.request_paint_only();
        }
        if matches!(event, Update::FocusChanged(false))
            && let Some(engine) = &self.engine
        {
            engine.clear_focus();
        }
    }

    fn on_anim_frame(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        interval: u64,
    ) {
        if let Some(leaf) = self.layout.leaf()
            && let Some(action) = leaf.frame(kithara_platform::time::Duration::from_nanos(interval))
        {
            ctx.submit_action::<HostAction>(action);
        }
        if let Some(leaf) = self.layout.leaf() {
            leaf.animate(ctx);
        }
    }

    fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        constraints: &BoxConstraints,
    ) -> MasonrySize {
        let limits = self.limits.unwrap_or_else(|| {
            solve::Limits::new(
                solve::Size::new(
                    constraints.min().width.as_(),
                    constraints.min().height.as_(),
                ),
                solve::Size::new(
                    constraints.max().width.as_(),
                    constraints.max().height.as_(),
                ),
            )
        });
        let size = self
            .layout
            .layout(ctx, &mut self.children, limits, self.declared);
        MasonrySize::new(f64::from(size.width), f64::from(size.height))
    }

    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        if let Some(geometry) = &self.geometry {
            geometry.set(ctx.bounding_rect());
        }
        if let Some(engine) = &self.engine {
            if let Some(area) = local_ime_area(engine, ctx.window_transform()) {
                ctx.set_ime_area(area);
            } else {
                ctx.clear_ime_area();
            }
        }
    }

    fn paint(&mut self, ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, scene: &mut Scene) {
        let size = ctx.size();
        let bounds = Rect {
            h: size.height.as_(),
            w: size.width.as_(),
            x: 0.0,
            y: 0.0,
        };
        let mut list = DrawListBuilder::default();
        self.paint_surface(bounds, &mut list);
        replay(&list.finish(), &mut VelloBackend::new(scene));
        if let Some(leaf) = self.layout.leaf() {
            leaf.paint(bounds, scene);
        }
    }

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        _node: &mut AccessNode,
    ) {
    }

    fn children_ids(&self) -> ChildrenIds {
        self.children.iter().map(WidgetPod::id).collect()
    }

    fn accepts_pointer_interaction(&self) -> bool {
        self.primary.is_some()
            || self.secondary.is_some()
            || self.layout.accepts_input()
            || self.engine.is_some()
    }

    fn accepts_focus(&self) -> bool {
        self.layout.accepts_input() || self.engine.is_some()
    }

    fn accepts_text_input(&self) -> bool {
        self.layout.accepts_text_input()
            || self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.accepts_text_input())
    }

    fn get_cursor(&self, ctx: &QueryCtx<'_>, pos: Point) -> CursorIcon {
        let local = ctx.window_transform().inverse() * pos;
        let size = ctx.size();
        let hit = Hit::new(
            Some(Pt {
                x: local.x.as_(),
                y: local.y.as_(),
            }),
            Rect {
                x: 0.0,
                y: 0.0,
                w: size.width.as_(),
                h: size.height.as_(),
            },
        );
        let leaf = match &self.layout {
            NodeLayout::Leaf(leaf) => leaf.cursor(&hit),
            NodeLayout::Flex(_) | NodeLayout::Stack => CursorShape::None,
        };
        let engine = self.engine.as_ref().map_or(CursorShape::None, |engine| {
            engine.cursor(Pt {
                x: pos.x.as_(),
                y: pos.y.as_(),
            })
        });
        let shape = match self.pointer_owner {
            Some(NodePointerOwner::Leaf) => leaf,
            None if leaf != CursorShape::None => leaf,
            Some(NodePointerOwner::Engine) | None => engine,
        };
        cursor_icon(shape)
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("KitharaMasonryNode", id = id.trace())
    }
}

fn sync_ime_area(ctx: &mut EventCtx<'_>, engine: &HostedEngine) {
    if let Some(area) = local_ime_area(engine, ctx.window_transform()) {
        ctx.set_ime_area(area);
    } else {
        ctx.clear_ime_area();
    }
}

fn local_ime_area(engine: &HostedEngine, transform: Affine) -> Option<MasonryRect> {
    engine.input_method_area().map(|area| {
        transform.inverse().transform_rect_bbox(MasonryRect::new(
            f64::from(area.x),
            f64::from(area.y),
            f64::from(area.x + area.w),
            f64::from(area.y + area.h),
        ))
    })
}

pub(crate) fn pointer_button(button: MasonryPointerButton) -> PointerButton {
    match button {
        MasonryPointerButton::Primary => PointerButton::Primary,
        MasonryPointerButton::Secondary => PointerButton::Secondary,
        MasonryPointerButton::Auxiliary => PointerButton::Auxiliary,
        MasonryPointerButton::X1 => PointerButton::Back,
        MasonryPointerButton::X2 => PointerButton::Forward,
        button => PointerButton::Other(button as u32),
    }
}

impl<Action> MasonryNode<Action> {
    pub(crate) fn document(
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
        let mut window = None;
        #[cfg(test)]
        let mut child_ids = Vec::new();
        for child in children {
            layers.extend(child.layers);
            popovers.extend(child.popovers);
            engine_targets.extend(child.engine_targets);
            engines.extend(child.engines);
            watched.extend(child.watched);
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
            window,
            #[cfg(test)]
            document_ids,
        }
    }

    pub(crate) fn furniture(
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
            window: None,
            #[cfg(test)]
            document_ids: Vec::new(),
        }
    }

    pub(crate) fn set_actions(
        &mut self,
        primary: Option<Box<dyn Fn() -> HostAction>>,
        secondary: Option<Box<dyn Fn() -> HostAction>>,
    ) {
        self.widget.widget.primary = primary;
        self.widget.widget.secondary = secondary;
    }

    delegate::delegate! {
        to self.layers {
            #[call(push)]
            pub(crate) fn add_layer(&mut self, layer: NewWidget<dyn Widget>);
            #[call(extend)]
            pub(crate) fn append_layers(&mut self, layers: Vec<NewWidget<dyn Widget>>);
        }
    }

    pub(crate) fn add_popover(
        &mut self,
        state: Rc<PopoverState>,
        dismiss: Rc<dyn Fn() -> HostAction>,
    ) {
        self.popovers.push((self.widget.id(), state, dismiss));
    }

    pub(crate) fn append_popovers(&mut self, popovers: Vec<PopoverRegistration>) {
        self.popovers.extend(popovers);
    }

    pub(crate) fn add_engine_control(&mut self, plan: HostedControlPlan) {
        let descriptors = plan.descriptors();
        if descriptors.is_empty() {
            return;
        }
        let geometry = self.track_geometry();
        self.engine_targets.push(EngineTarget {
            area: geometry,
            plan,
        });
    }

    pub(crate) fn track_geometry(&mut self) -> Rc<Cell<MasonryRect>> {
        if let Some(geometry) = &self.widget.widget.geometry {
            return Rc::clone(geometry);
        }
        let geometry = Rc::new(Cell::new(MasonryRect::ZERO));
        self.widget.widget.geometry = Some(Rc::clone(&geometry));
        geometry
    }

    pub(crate) fn append_engine_targets(&mut self, targets: Vec<EngineTarget>) {
        self.engine_targets.extend(targets);
    }

    pub(crate) fn append_engines(&mut self, engines: Vec<Rc<HostedEngine>>) {
        self.engines.extend(engines);
    }

    pub(crate) fn append_watched(&mut self, watched: Vec<Watched>) {
        self.watched.extend(watched);
    }

    /// Remembers that this node's leaf shows one endpoint, so its value can be
    /// re-read into the mounted tree instead of rebuilding the tree to show it.
    pub(crate) fn watch(&mut self, binding: &Binding) {
        self.watched.push((self.widget.id(), binding.clone()));
    }

    pub(crate) fn host_engine(&mut self, map_event: Rc<dyn Fn(UiEvent) -> HostAction>) {
        if self.engine_targets.is_empty() {
            return;
        }
        let targets = std::mem::take(&mut self.engine_targets);
        let engine = HostedEngine::new(self.widget.id(), targets, map_event);
        self.engines.push(Rc::clone(&engine));
        self.widget.widget.engine = Some(engine);
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
