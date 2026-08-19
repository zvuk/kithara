use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use masonry::{
    accesskit::{Node as AccessNode, Role, TreeUpdate},
    app::{RenderRoot, RenderRootOptions, RenderRootSignal},
    core::{
        AccessCtx, BoxConstraints, ChildrenIds, CursorIcon, EventCtx, Handled, LayoutCtx, PaintCtx,
        PointerEvent, PropertiesMut, PropertiesRef, QueryCtx, RegisterCtx, TextEvent, Widget,
        WidgetId, WidgetRef, WindowEvent, find_widget_under_pointer,
    },
    kurbo::{Point, Size},
    ui_events::keyboard::{Key, NamedKey},
    vello::Scene,
};
use num_traits::cast::AsPrimitive;
use thiserror::Error;
use tracing::{Span, trace_span};

use super::{
    built::{MasonryNode, RootParts, Watched},
    custom::HostAction,
    leaf::cursor_icon,
    node::Node,
    picker::{self, HostedEngine},
    popover::PopoverState,
};
use crate::{
    backends::VelloBackend,
    draw::{Pt, Rect, replay},
    interact::CursorShape,
    render::{
        DragGhost, Skin, UiEvent, WindowCommand, WindowSurface,
        document::{Ctx, placements},
        shader::ShaderDeclaration,
        vis::VisDeclaration,
    },
    shaping::TextContext,
};

type WindowTracker = (Rc<Cell<Option<Pt>>>, Option<WidgetId>, bool);
type PopoverRegistration = (WidgetId, Rc<PopoverState>, Rc<dyn Fn() -> HostAction>);

/// A Masonry render root with native-layer synchronization and typed actions.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct MasonryRoot<Action> {
    actions: Vec<Action>,
    /// The document this root holds draws a different picture at a later
    /// moment, so a frame it finished has to be followed by another.
    ///
    /// The tree answers for the widgets it owns: a leaf that repaints of its own
    /// accord asks for the next frame through it. What the tree cannot answer is
    /// the document, whose values are read afresh against a clock the host
    /// advances — the tree is only ever told what this frame turned out to be,
    /// never that the next one will differ. So the document says so here, or it
    /// is drawn once and then only when something unrelated wakes the window.
    #[field(with, vis = "pub")]
    animates: bool,
    platform: Vec<RenderRootSignal>,
    engines: Vec<Rc<HostedEngine>>,
    popovers: Vec<PopoverRegistration>,
    watched: Vec<Watched>,
    native: Vec<WidgetId>,
    window: Option<WindowTracker>,
    #[field(get, vis = "pub")]
    root: RenderRoot,
    signals: Rc<RefCell<VecDeque<RenderRootSignal>>>,
}

/// Failure to recover the host's declared action type from a Masonry signal.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MasonryRootError {
    #[error("foreign Masonry widget emitted action type `{actual}`")]
    ForeignAction { actual: &'static str },
    #[error("Masonry host mapped `{actual}` while the root expects `{expected}`")]
    MappedAction {
        actual: &'static str,
        expected: &'static str,
    },
}

impl<Action> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// Creates the real render root, installs its native layers, and owns all signals.
    ///
    /// # Errors
    ///
    /// Returns [`MasonryRootError`] if a foreign widget emits an action that
    /// cannot be recovered as the declared host action.
    pub fn new(
        node: MasonryNode<Action>,
        options: RenderRootOptions,
    ) -> Result<Self, MasonryRootError> {
        let (base, layers, popovers, engines, native, window, watched) = RootParts::from(node);
        let signals = Rc::new(RefCell::new(VecDeque::new()));
        let sink = Rc::clone(&signals);
        let root = RenderRoot::new(
            base,
            move |signal| sink.borrow_mut().push_back(signal),
            options,
        );
        let mut this = Self {
            actions: Vec::new(),
            animates: false,
            platform: Vec::new(),
            engines,
            popovers,
            watched,
            native,
            window,
            root,
            signals,
        };
        this.sync_popovers();
        for layer in layers {
            this.root.add_layer(layer, Point::ORIGIN);
        }
        this.sync_popovers();
        this.sync()?;
        Ok(this)
    }

    pub(crate) fn vis_declarations(&self) -> Vec<VisDeclaration> {
        self.native
            .iter()
            .filter_map(|id| {
                let widget = self.root.get_widget(*id)?.downcast::<Node>()?;
                if widget.ctx().is_stashed() {
                    return None;
                }
                let frame = widget.vis_frame()?;
                let bounds = widget.ctx().bounding_rect();
                VisDeclaration::logical(frame, [bounds.x0, bounds.y0, bounds.x1, bounds.y1])
            })
            .collect()
    }

    pub(crate) fn shader_declarations(&self) -> Vec<ShaderDeclaration> {
        self.native
            .iter()
            .filter_map(|id| {
                let widget = self.root.get_widget(*id)?.downcast::<Node>()?;
                if widget.ctx().is_stashed() {
                    return None;
                }
                widget.shader_declaration()
            })
            .collect()
    }

    /// Dispatches one keyboard or input-method event.
    ///
    /// # Errors
    ///
    /// Returns [`MasonryRootError`] when a widget violates the typed action contract.
    pub fn handle_text_event(&mut self, event: TextEvent) -> Result<Handled, MasonryRootError> {
        let dismiss = matches!(
            &event,
            TextEvent::Keyboard(event)
                if event.state.is_down() && event.key == Key::Named(NamedKey::Escape)
        );
        let handled = self.root.handle_text_event(event);
        self.sync()?;
        if handled == Handled::No
            && dismiss
            && let Some(action) = self
                .popovers
                .iter()
                .rev()
                .find(|(_, state, _)| state.is_open())
                .map(|(_, _, dismiss)| dismiss())
        {
            self.push_action(Box::new(action))?;
            return Ok(Handled::Yes);
        }
        Ok(handled)
    }

    /// Dispatches a window event and reflows positioned layers after resize.
    ///
    /// # Errors
    ///
    /// Returns [`MasonryRootError`] when a widget violates the typed action contract.
    pub fn handle_window_event(&mut self, event: WindowEvent) -> Result<Handled, MasonryRootError> {
        let handled = self.root.handle_window_event(event);
        self.sync_popovers();
        self.sync()?;
        Ok(handled)
    }

    /// Renders the current tree and applies signals emitted during paint.
    ///
    /// # Errors
    ///
    /// Returns [`MasonryRootError`] when a widget violates the typed action contract.
    pub fn redraw(&mut self) -> Result<(Scene, Option<TreeUpdate>), MasonryRootError> {
        let rendered = self.root.redraw();
        self.sync()?;
        Ok(rendered)
    }

    /// Takes all concrete actions emitted since the previous call.
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    /// Takes non-layer, non-action signals for the platform runner.
    pub fn take_platform_signals(&mut self) -> Vec<RenderRootSignal> {
        std::mem::take(&mut self.platform)
    }

    /// Satisfies the redraw signals covered by a completed frame.
    ///
    /// Returns whether an animation frame, rather than an ordinary redraw, was
    /// requested. Every unrelated platform signal remains queued in order.
    pub(crate) fn complete_frame(&mut self) -> bool {
        complete_frame_signals(&mut self.platform) || self.animates
    }

    /// Reports whether Masonry requested another paint or animation frame.
    pub(crate) fn needs_frame(&self) -> bool {
        frame_requested(&self.platform) || self.animates
    }

    #[cfg(test)]
    pub(crate) fn tree_picture(&self, path: &str) -> Option<(usize, String)> {
        self.engines
            .iter()
            .find_map(|engine| engine.tree_picture(path))
    }

    fn sync_popovers(&self) {
        for (id, state, _) in &self.popovers {
            if let Some(widget) = self.root.get_widget(*id) {
                state.set_anchor(widget.ctx().bounding_rect());
            }
        }
    }

    /// Repaints the layer of every engine whose menu has changed.
    ///
    /// The menu is drawn above the tree by a layer of its own, so the widget
    /// that routed the press cannot mark it: a press that opens a menu is,
    /// from Masonry's side, a press that changed nothing on screen.
    fn sync_menus(&mut self) {
        let changed: Vec<WidgetId> = self
            .engines
            .iter()
            .filter_map(|engine| engine.take_changed_menu())
            .collect();
        for layer in changed {
            self.root.edit_widget(layer, |mut layer| {
                layer.ctx.request_paint_only();
            });
        }
    }

    fn sync(&mut self) -> Result<(), MasonryRootError> {
        self.sync_menus();
        loop {
            let pending = {
                let mut signals = self.signals.borrow_mut();
                if signals.is_empty() {
                    break;
                }
                signals.drain(..).collect::<Vec<_>>()
            };
            for signal in pending {
                match signal {
                    RenderRootSignal::Action(action, _) => self.push_action(action)?,
                    RenderRootSignal::NewLayer(layer, position) => {
                        self.root.add_layer(layer, position);
                    }
                    RenderRootSignal::RemoveLayer(id) => self.root.remove_layer(id),
                    RenderRootSignal::RepositionLayer(id, position) => {
                        self.root.reposition_layer(id, position);
                    }
                    signal => self.platform.push(signal),
                }
            }
        }
        Ok(())
    }

    fn push_action(&mut self, action: masonry::core::ErasedAction) -> Result<(), MasonryRootError> {
        let actual = action.type_name();
        let host = action
            .downcast::<HostAction>()
            .map_err(|_| MasonryRootError::ForeignAction { actual })?;
        let actual = host.type_name();
        let action = (*host)
            .downcast::<Action>()
            .map_err(|_| MasonryRootError::MappedAction {
                actual,
                expected: std::any::type_name::<Action>(),
            })?;
        self.actions.push(action);
        Ok(())
    }
}

/// How a pointer event finds the widget that answers it.
///
/// The tree answers by hit-testing, which is enough for a control that owns
/// the box under the hand. The rest lives here: the window chrome that
/// answers before the tree, the menu and the drag that outlive the box they
/// started in, and the popover a press outside dismisses.
impl<Action> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// Dispatches one pointer event, including origin banking and layer updates.
    ///
    /// # Errors
    ///
    /// Returns [`MasonryRootError`] when a widget violates the typed action contract.
    pub fn handle_pointer_event(
        &mut self,
        event: PointerEvent,
    ) -> Result<Handled, MasonryRootError> {
        self.observe_pointer(&event);
        if self.root.pointer_capture_target().is_some() || self.window_answers_first(&event) {
            return self.route_root_pointer(event);
        }
        if self.routes_open_picker(&event)? {
            self.sync()?;
            return Ok(Handled::Yes);
        }
        if self.dismisses_popover(&event)? {
            return Ok(Handled::Yes);
        }
        let at = picker::input(&event).map(|(_, point)| point);
        self.routes_item_drag(&event)?;
        self.bank_pointer(&event);
        let handled = self.route_root_pointer(event)?;
        if let Some(point) = at {
            self.show_item_drag(point);
        }
        Ok(handled)
    }

    /// Says what the hand is doing while it carries an item.
    ///
    /// The tree asks whatever sits under the pointer, and by the time a track
    /// is over the deck that is the deck, which knows nothing about the drag.
    /// The list that started it does, so its answer is the last word.
    fn show_item_drag(&mut self, point: Pt) {
        let Some(engine) = self.item_drag_engine() else {
            return;
        };
        let shape = engine.cursor(point);
        if shape == CursorShape::None {
            return;
        }
        self.platform
            .push(RenderRootSignal::SetCursor(cursor_icon(shape)));
    }

    /// The list a hand is currently pulling an item out of, if any.
    fn item_drag_engine(&self) -> Option<Rc<HostedEngine>> {
        self.engines
            .iter()
            .find(|engine| engine.holds_item())
            .map(Rc::clone)
    }

    /// Keeps feeding the pointer to the list a hand is pulling an item out of.
    ///
    /// The gesture never takes the pointer, so a tree that delivers events by
    /// hit-testing loses the drag the moment the hand leaves the list, and the
    /// release lands on whatever sits under it instead. The list is the one
    /// place that knows the gesture is still running, so while it says so the
    /// root hands it the event as well. Only while the hand is outside the box
    /// the tree would have reached on its own: inside it, the list already
    /// hears everything, and a second copy would be a second gesture.
    ///
    /// This does not answer the event. Whatever the hand is over still sees it,
    /// which is how a module that takes drops learns the hand is above it.
    fn routes_item_drag(&mut self, event: &PointerEvent) -> Result<(), MasonryRootError> {
        let Some((input, point)) = picker::input(event) else {
            return Ok(());
        };
        let Some(engine) = self.item_drag_engine() else {
            return Ok(());
        };
        if self.owns_point(engine.owner(), point) {
            return Ok(());
        }
        if let Some(action) = engine.route(input, Some(point)).outcome.value() {
            self.push_action(Box::new(action))?;
        }
        Ok(())
    }

    /// Whether the tree would deliver a pointer at this point to that widget.
    fn owns_point(&self, owner: WidgetId, point: Pt) -> bool {
        self.root.get_widget(owner).is_some_and(|widget| {
            widget
                .ctx()
                .bounding_rect()
                .contains(Point::new(point.x.into(), point.y.into()))
        })
    }

    fn route_root_pointer(&mut self, event: PointerEvent) -> Result<Handled, MasonryRootError> {
        let handled = self.root.handle_pointer_event(event);
        self.sync_popovers();
        self.sync()?;
        Ok(handled)
    }

    fn routes_open_picker(&mut self, event: &PointerEvent) -> Result<bool, MasonryRootError> {
        let Some((input, point)) = picker::input(event) else {
            return Ok(false);
        };
        let Some(engine) = self
            .engines
            .iter()
            .rev()
            .find(|engine| engine.has_open_picker())
            .map(Rc::clone)
        else {
            return Ok(false);
        };
        let owner = engine.owner();
        let routed = engine.route(input, Some(point));
        let (outcome, focused) = (routed.outcome, routed.focused);
        self.sync_picker_focus(owner, focused);
        let captured = outcome.is_captured();
        if let Some(action) = outcome.value() {
            self.push_action(Box::new(action))?;
        }
        Ok(captured)
    }

    fn sync_picker_focus(&mut self, owner: WidgetId, focused: bool) {
        if focused {
            self.root.focus_on(Some(owner));
        } else if self.root.focused_widget() == Some(owner) {
            self.root.focus_on(None);
        }
    }

    fn observe_pointer(&mut self, event: &PointerEvent) {
        let position = match event {
            PointerEvent::Down(button) | PointerEvent::Up(button) => {
                Some(button.state.logical_position())
            }
            PointerEvent::Move(update) => Some(update.current.logical_position()),
            PointerEvent::Scroll(scroll) => Some(scroll.state.logical_position()),
            PointerEvent::Gesture(gesture) => Some(gesture.state.logical_position()),
            PointerEvent::Cancel(_) | PointerEvent::Enter(_) | PointerEvent::Leave(_) => None,
        };
        if let Some(position) = position {
            let point = Pt {
                x: position.x.as_(),
                y: position.y.as_(),
            };
            if let Some((pointer, layer, repaint)) = &self.window {
                pointer.set(Some(point));
                if *repaint
                    && matches!(event, PointerEvent::Move(_))
                    && let Some(layer) = layer
                {
                    let layer = *layer;
                    self.root.edit_widget(layer, |mut layer| {
                        layer.ctx.request_paint_only();
                    });
                }
            }
        }
    }

    fn bank_pointer(&self, event: &PointerEvent) {
        let PointerEvent::Down(button) = event else {
            return;
        };
        if self.popovers.iter().any(|(_, state, _)| state.is_open()) {
            return;
        }
        let position = button.state.logical_position();
        let point = Point::new(position.x, position.y);
        if self.window_owns(point) {
            return;
        }
        for (_, state, _) in &self.popovers {
            state.bank(point);
        }
    }

    fn dismisses_popover(&mut self, event: &PointerEvent) -> Result<bool, MasonryRootError> {
        let PointerEvent::Down(button) = event else {
            return Ok(false);
        };
        let Some((_, state, dismiss)) = self
            .popovers
            .iter()
            .rev()
            .find(|(_, state, _)| state.is_open())
        else {
            return Ok(false);
        };
        let position = button.state.logical_position();
        let point = Point::new(position.x, position.y);
        if self.window_owns(point) || state.surface().contains(point) {
            return Ok(false);
        }
        let action = dismiss();
        self.push_action(Box::new(action))?;
        Ok(true)
    }

    fn window_owns(&self, point: Point) -> bool {
        let Some((_, Some(layer), _)) = &self.window else {
            return false;
        };
        self.root
            .get_widget(*layer)
            .and_then(|layer| layer.find_widget_under_pointer(point))
            .is_some()
    }

    fn window_answers_first(&self, event: &PointerEvent) -> bool {
        let PointerEvent::Down(button) = event else {
            return false;
        };
        let position = button.state.logical_position();
        self.window_owns(Point::new(position.x, position.y))
    }
}

/// Re-reading a standing tree, which is what this host does instead of
/// rebuilding one.
impl<Action> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// Re-reads everything the mounted document shows and hands it to the
    /// widget that draws it.
    ///
    /// This is what a rebuild was doing, minus the rebuild: the tree stays, so a
    /// gesture in flight and the pointer capture that feeds it both survive, and
    /// every control bound to the same endpoint moves together rather than one
    /// of them being poked by hand.
    ///
    /// Two kinds of thing change between frames without the document changing.
    /// A control's *value* comes from an endpoint the control names, and is
    /// re-read one control at a time. A control's *pose* comes from the objects
    /// around it, and is worked out by the document walk rather than named
    /// anywhere, so it takes a walk to re-read — one for the whole document.
    pub fn refresh(&mut self, ctx: Ctx<'_, '_>) {
        self.show_values(ctx);
        self.reread_plans(ctx);
        self.place_objects(ctx);
    }

    /// Carries the frame just read into the gestures already mounted.
    ///
    /// A control answers a hand against what it is showing, and what it is
    /// showing changes without the tree changing shape. The immediate host
    /// resolves that afresh every frame because it rebuilds; this one re-reads
    /// it in place.
    fn reread_plans(&mut self, ctx: Ctx<'_, '_>) {
        for engine in &self.engines {
            engine.reread(ctx);
        }
    }

    fn show_values(&mut self, ctx: Ctx<'_, '_>) {
        for watched in &self.watched {
            match watched {
                Watched::Read { id, binding } => {
                    let Some(value) = ctx.read(binding) else {
                        continue;
                    };
                    self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        if node.widget.show_live(&value) {
                            node.ctx.request_paint_only();
                        }
                    });
                }
                Watched::Snapshot { id } => {
                    self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        if node.widget.refresh(ctx) {
                            node.ctx.request_paint_only();
                        }
                    });
                }
                Watched::Placed { .. } => {}
            }
        }
    }

    /// Walks the document again for the poses alone, and moves whatever the
    /// walk now puts somewhere else.
    ///
    /// Nothing is watched this way unless the document declares an object an
    /// endpoint drives, so a page that never moves pays for none of this.
    fn place_objects(&mut self, ctx: Ctx<'_, '_>) {
        if !ctx.ui.driven {
            return;
        }
        let placed = placements(&ctx.ui.root, ctx);
        for watched in &self.watched {
            let Watched::Placed { id, path } = watched else {
                continue;
            };
            let Some(transform) = placed.get(path).copied() else {
                continue;
            };
            self.root.edit_widget(*id, |mut widget| {
                let mut node = widget.downcast::<Node>();
                if node.widget.place(transform) {
                    node.ctx.request_paint_only();
                }
            });
        }
    }
}

fn complete_frame_signals(signals: &mut Vec<RenderRootSignal>) -> bool {
    let mut animation = false;
    signals.retain(|signal| match signal {
        RenderRootSignal::RequestRedraw => false,
        RenderRootSignal::RequestAnimFrame => {
            animation = true;
            false
        }
        _ => true,
    });
    animation
}

fn frame_requested(signals: &[RenderRootSignal]) -> bool {
    signals.iter().any(|signal| {
        matches!(
            signal,
            RenderRootSignal::RequestRedraw | RenderRootSignal::RequestAnimFrame
        )
    })
}

pub(crate) struct WindowLayer {
    active: Option<WindowCommand>,
    ghost: Option<DragGhost>,
    map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    pointer: Rc<Cell<Option<Pt>>>,
    resize_edge: f32,
    resize_edges: bool,
    text: TextContext,
}

impl WindowLayer {
    pub(crate) fn new(
        dragged: Option<String>,
        resize_edges: bool,
        pointer: Rc<Cell<Option<Pt>>>,
        map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
        skin: &Skin,
    ) -> Self {
        Self {
            active: None,
            ghost: dragged.map(|label| DragGhost::new(Some(label), skin)),
            map_event,
            pointer,
            resize_edge: skin.window.resize_edge,
            resize_edges,
            text: TextContext::from(skin.text_resources()),
        }
    }

    fn bounds(size: Size) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: size.width.as_(),
            h: size.height.as_(),
        }
    }

    fn resize_layer(&self, size: Size) -> Option<crate::render::HostLayer<WindowCommand>> {
        self.resize_edges
            .then(|| WindowSurface::frame(Self::bounds(size), self.resize_edge))
    }

    fn command_at(&self, size: Size, pointer: Option<Pt>) -> Option<WindowCommand> {
        self.resize_layer(size)
            .and_then(|layer| layer.action_at(pointer).copied())
    }
}

impl Widget for WindowLayer {
    type Action = HostAction;

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if let PointerEvent::Down(button) = event {
            let position = button.state.logical_position();
            let pointer = Some(Pt {
                x: position.x.as_(),
                y: position.y.as_(),
            });
            if let Some(command) = self.command_at(ctx.size(), pointer) {
                self.active = Some(command);
                ctx.submit_action::<HostAction>((self.map_event)(UiEvent::Window(command)));
                ctx.capture_pointer();
                ctx.set_handled();
                return;
            }
        }
        if ctx.is_pointer_capture_target() {
            if matches!(event, PointerEvent::Move(_)) {
                ctx.request_paint_only();
            }
            if matches!(event, PointerEvent::Up(_) | PointerEvent::Cancel(_)) {
                self.active = None;
                ctx.release_pointer();
                ctx.request_cursor_icon_change();
            }
            ctx.set_handled();
        }
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn layout(
        &mut self,
        _ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        constraints: &BoxConstraints,
    ) -> Size {
        constraints.max()
    }

    fn paint(&mut self, ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, scene: &mut Scene) {
        let Some(ghost) = &self.ghost else {
            return;
        };
        let layer = ghost.layer(self.pointer.get(), Self::bounds(ctx.size()), &mut self.text);
        replay(layer.draw(), &mut VelloBackend::new(scene));
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
        ChildrenIds::new()
    }

    fn get_cursor(&self, ctx: &QueryCtx<'_>, pos: Point) -> CursorIcon {
        let local = ctx.window_transform().inverse() * pos;
        let pointer = Some(Pt {
            x: local.x.as_(),
            y: local.y.as_(),
        });
        let cursor = self.active.map_or_else(
            || {
                self.resize_layer(ctx.size())
                    .map_or(CursorShape::None, |layer| layer.cursor_at(pointer))
            },
            command_cursor,
        );
        cursor_icon(cursor)
    }

    fn find_widget_under_pointer<'ctx>(
        &'ctx self,
        ctx: QueryCtx<'ctx>,
        pos: Point,
    ) -> Option<WidgetRef<'ctx, dyn Widget>> {
        let local = ctx.window_transform().inverse() * pos;
        let pointer = Some(Pt {
            x: local.x.as_(),
            y: local.y.as_(),
        });
        self.command_at(ctx.size(), pointer)
            .and_then(|_| find_widget_under_pointer(self, ctx, pos))
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("KitharaWindowLayer", id = id.trace())
    }
}

const fn command_cursor(command: WindowCommand) -> CursorShape {
    match command {
        WindowCommand::Resize(
            crate::render::WindowEdge::North | crate::render::WindowEdge::South,
        ) => CursorShape::ResizeV,
        WindowCommand::Resize(
            crate::render::WindowEdge::East | crate::render::WindowEdge::West,
        ) => CursorShape::ResizeH,
        WindowCommand::Resize(
            crate::render::WindowEdge::NorthWest | crate::render::WindowEdge::SouthEast,
        ) => CursorShape::ResizeDiagonalDown,
        WindowCommand::Resize(
            crate::render::WindowEdge::NorthEast | crate::render::WindowEdge::SouthWest,
        ) => CursorShape::ResizeDiagonalUp,
        WindowCommand::Drag
        | WindowCommand::Minimize
        | WindowCommand::ToggleMaximize
        | WindowCommand::ToggleFullScreen
        | WindowCommand::Close => CursorShape::None,
    }
}

#[cfg(test)]
mod frame_signal_tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn ordinary_redraw_does_not_schedule_another_frame() {
        let mut signals = vec![RenderRootSignal::RequestRedraw];

        assert!(!complete_frame_signals(&mut signals));
        assert!(signals.is_empty());
    }

    #[kithara::test]
    fn animation_request_schedules_another_frame() {
        let mut signals = vec![RenderRootSignal::RequestAnimFrame];

        assert!(complete_frame_signals(&mut signals));
        assert!(signals.is_empty());
    }

    #[kithara::test]
    fn only_redraw_and_animation_signals_need_a_frame() {
        let no_frame = [RenderRootSignal::StartIme, RenderRootSignal::EndIme];
        let redraw = [RenderRootSignal::RequestRedraw];
        let animation = [RenderRootSignal::RequestAnimFrame];

        assert!(!frame_requested(&no_frame));
        assert!(frame_requested(&redraw));
        assert!(frame_requested(&animation));
    }

    #[kithara::test]
    fn satisfied_signals_are_removed_and_unrelated_signals_keep_their_order() {
        let mut signals = vec![
            RenderRootSignal::StartIme,
            RenderRootSignal::RequestRedraw,
            RenderRootSignal::EndIme,
            RenderRootSignal::RequestAnimFrame,
            RenderRootSignal::TakeFocus,
        ];

        assert!(complete_frame_signals(&mut signals));
        assert!(matches!(
            signals.as_slice(),
            [
                RenderRootSignal::StartIme,
                RenderRootSignal::EndIme,
                RenderRootSignal::TakeFocus
            ]
        ));
    }
}
