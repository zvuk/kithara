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
    custom::HostAction,
    node::{MasonryNode, Node, RootParts, Watched},
    picker::{self, HostedEngine},
    popover::PopoverState,
};
use crate::{
    backends::VelloBackend,
    compile::CompiledUi,
    draw::{Pt, Rect, replay},
    interact::CursorShape,
    render::{Reads, Skin, UiEvent, WindowCommand, document::read::resolve},
    text::TextContext,
    widgets::{drag_ghost::DragGhost, window::WindowSurface},
};

type WindowTracker = (Rc<Cell<Option<Pt>>>, Option<WidgetId>, bool);
type PopoverRegistration = (WidgetId, Rc<PopoverState>, Rc<dyn Fn() -> HostAction>);

/// A Masonry render root with native-layer synchronization and typed actions.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct MasonryRoot<Action> {
    actions: Vec<Action>,
    platform: Vec<RenderRootSignal>,
    engines: Vec<Rc<HostedEngine>>,
    popovers: Vec<PopoverRegistration>,
    watched: Vec<Watched>,
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
        let (base, layers, popovers, engines, window, watched) = RootParts::from(node);
        let signals = Rc::new(RefCell::new(VecDeque::new()));
        let sink = Rc::clone(&signals);
        let root = RenderRoot::new(
            base,
            move |signal| sink.borrow_mut().push_back(signal),
            options,
        );
        let mut this = Self {
            actions: Vec::new(),
            platform: Vec::new(),
            engines,
            popovers,
            watched,
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
            return Ok(Handled::Yes);
        }
        if self.dismisses_popover(&event)? {
            return Ok(Handled::Yes);
        }
        self.bank_pointer(&event);
        self.route_root_pointer(event)
    }

    fn route_root_pointer(&mut self, event: PointerEvent) -> Result<Handled, MasonryRootError> {
        let handled = self.root.handle_pointer_event(event);
        self.sync_popovers();
        self.sync()?;
        Ok(handled)
    }

    /// Re-reads every endpoint the mounted document shows and hands each value
    /// to the widget that draws it.
    ///
    /// This is what a rebuild was doing, minus the rebuild: the tree stays, so a
    /// gesture in flight and the pointer capture that feeds it both survive, and
    /// every control bound to the same endpoint moves together rather than one
    /// of them being poked by hand.
    pub fn refresh(&mut self, ui: &CompiledUi, reads: &dyn Reads) {
        for (id, binding) in &self.watched {
            let Some(value) = resolve(reads, binding, ui) else {
                continue;
            };
            self.root.edit_widget(*id, |mut widget| {
                let mut node = widget.downcast::<Node>();
                if node.widget.show_live(&value) {
                    node.ctx.request_paint_only();
                }
            });
        }
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

    fn sync_popovers(&self) {
        for (id, state, _) in &self.popovers {
            if let Some(widget) = self.root.get_widget(*id) {
                state.set_anchor(widget.ctx().bounding_rect());
            }
        }
    }

    fn sync(&mut self) -> Result<(), MasonryRootError> {
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

const fn cursor_icon(shape: CursorShape) -> CursorIcon {
    match shape {
        CursorShape::None => CursorIcon::Default,
        CursorShape::Grab => CursorIcon::Grab,
        CursorShape::Grabbing => CursorIcon::Grabbing,
        CursorShape::Pointer => CursorIcon::Pointer,
        CursorShape::ResizeDiagonalDown => CursorIcon::NwseResize,
        CursorShape::ResizeDiagonalUp => CursorIcon::NeswResize,
        CursorShape::ResizeH => CursorIcon::EwResize,
        CursorShape::ResizeV => CursorIcon::NsResize,
        CursorShape::Text => CursorIcon::Text,
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
        | WindowCommand::Close => CursorShape::None,
    }
}
