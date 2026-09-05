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
    kurbo::{Point, Rect as MasonryRect, Size},
    ui_events::keyboard::{Key, NamedKey},
    vello::Scene,
};
use num_traits::cast::AsPrimitive;
use thiserror::Error;
use tracing::{Span, trace_span};

use super::{
    built::{
        BlockRegistration, MasonryNode, NodeBox, PopoverRegistration, RootParts, Watched,
        WindowTracker,
    },
    custom::HostAction,
    leaf::cursor_icon,
    node::Node,
    picker::{self, HostedEngine},
};
#[cfg(any(test, feature = "capture"))]
use crate::draw::Rgba;
use crate::{
    backends::VelloBackend,
    draw::{Pt, Rect, replay},
    interact::CursorShape,
    render::{
        DragGhost, Skin, UiEvent, WindowCommand, WindowSurface, shader::ShaderDeclaration,
        vis::VisDeclaration,
    },
    shaping::TextContext,
};

#[path = "../masonry_tree/reread.rs"]
mod reread;

/// A Masonry render root with native-layer synchronization and typed actions.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct MasonryRoot<Action> {
    window: Option<WindowTracker>,
    signals: Rc<RefCell<VecDeque<RenderRootSignal>>>,
    #[field(get, vis = "pub")]
    root: RenderRoot,
    actions: Vec<Action>,
    blocks: Vec<BlockRegistration>,
    boxes: Vec<NodeBox>,
    engines: Vec<Rc<HostedEngine>>,
    native: Vec<WidgetId>,
    platform: Vec<RenderRootSignal>,
    popovers: Vec<PopoverRegistration>,
    watched: Vec<Watched>,
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
    /// Whether the press the window last reported was a second one, so the
    /// release that follows it is a double click rather than a plain one.
    double_click: bool,
    /// The last refresh moved the picture without an event having asked it to.
    ///
    /// A document showing values the application keeps changing draws a new
    /// picture whenever those values move, and nothing in the tree knows that
    /// the next one will differ again. So a frame that moved asks for the one
    /// after it, and a document that has come to rest stops asking after a
    /// single frame that moved nothing.
    moved: bool,
    /// What the window last said its scale is, which is what turns a wheel's
    /// physical delta into the distance a control scrolls.
    scale: f64,
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
        let (base, layers, popovers, blocks, engines, boxes, native, window, watched) =
            RootParts::from(node);
        let scale = options.scale_factor;
        let signals = Rc::new(RefCell::new(VecDeque::new()));
        let sink = Rc::clone(&signals);
        let root = RenderRoot::new(
            base,
            move |signal| sink.borrow_mut().push_back(signal),
            options,
        );
        let mut this = Self {
            engines,
            scale,
            boxes,
            popovers,
            blocks,
            watched,
            native,
            window,
            root,
            signals,
            actions: Vec::new(),
            animates: false,
            moved: false,
            platform: Vec::new(),
            double_click: false,
        };
        this.sync_popovers();
        for layer in layers {
            this.root.add_layer(layer, Point::ORIGIN);
        }
        this.sync_popovers();
        this.sync()?;
        Ok(this)
    }

    /// Where the node a surface hangs on stands, or nothing when it stands
    /// nowhere.
    ///
    /// A node the room did not reach is stashed rather than left out of the
    /// tree, and a stashed node keeps the box it last had, so the box alone
    /// cannot say whether it is still in the picture.
    fn anchor_box(&self, anchor: WidgetId) -> Option<MasonryRect> {
        let widget = self.root.get_widget(anchor)?;
        (!widget.ctx().is_stashed()).then(|| widget.ctx().bounding_rect())
    }

    /// Satisfies the redraw signals covered by a completed frame.
    ///
    /// Returns whether an animation frame, rather than an ordinary redraw, was
    /// requested. Every unrelated platform signal remains queued in order.
    pub(crate) fn complete_frame(&mut self) -> bool {
        let animation = complete_frame_signals(&mut self.platform);
        animation || self.animates || std::mem::take(&mut self.moved)
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
                .find(|popover| popover.state.standing().is_some())
                .map(|popover| (popover.dismiss)())
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
        if let WindowEvent::Rescale(scale) = &event {
            self.scale = *scale;
        }
        let handled = self.root.handle_window_event(event);
        self.sync_popovers();
        self.sync()?;
        Ok(handled)
    }

    /// Reports whether Masonry requested another paint or animation frame.
    pub(crate) fn needs_frame(&self) -> bool {
        frame_requested(&self.platform) || self.animates || self.moved
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

    /// The colour the node `id` writes its text in right now.
    #[cfg(any(test, feature = "capture"))]
    pub(crate) fn ink_of(&self, id: WidgetId) -> Option<Rgba> {
        self.root.get_widget(id)?.downcast::<Node>()?.ink()
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

    fn sync(&mut self) -> Result<(), MasonryRootError> {
        self.sync_boxes();
        self.sync_menus();
        loop {
            let pending = {
                let mut signals = self.signals.borrow_mut();
                if signals.is_empty() {
                    break;
                }
                signals.drain(..).collect::<Vec<RenderRootSignal>>()
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

    /// Re-reads the box every mounted surface that answers a hand stands in.
    ///
    /// A control an engine drives answers the pointer against a box, and that
    /// box moves without the control being told: Masonry recomputes a whole
    /// subtree itself when a window above it scrolls and calls no widget back.
    /// So the boxes are read out of the tree here, after every event, the same
    /// way a popover reads its anchor.
    fn sync_boxes(&self) {
        for stood in &self.boxes {
            stood
                .area
                .set(self.anchor_box(stood.node).unwrap_or(MasonryRect::ZERO));
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

    fn sync_popovers(&self) {
        for popover in &self.popovers {
            popover.state.set_anchor(self.anchor_box(popover.anchor));
        }
    }

    /// Takes all concrete actions emitted since the previous call.
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    /// Takes the cursor the tree last asked its window to show, if it asked.
    ///
    /// Every cursor the tree resolves reaches a host this way: the shape a
    /// widget answers a hover with, the resize edge, the text caret, and the
    /// shape [`Self::show_cursor`] takes from a router. A host that never reads
    /// them shows one cursor for the life of its window and keeps them queued
    /// forever, so a window runner takes them here. Only the last one is worth
    /// showing, and every other platform signal is left where it stands.
    pub fn take_cursor(&mut self) -> Option<CursorIcon> {
        let mut cursor = None;
        self.platform.retain(|signal| {
            let RenderRootSignal::SetCursor(icon) = signal else {
                return true;
            };
            cursor = Some(*icon);
            false
        });
        cursor
    }

    /// Takes non-layer, non-action signals for the platform runner.
    pub fn take_platform_signals(&mut self) -> Vec<RenderRootSignal> {
        std::mem::take(&mut self.platform)
    }

    #[cfg(test)]
    pub(crate) fn tree_picture(&self, path: &str) -> Option<(usize, String)> {
        self.engines
            .iter()
            .find_map(|engine| engine.tree_picture(path))
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
}

/// How a pointer event finds the control that answers it.
///
/// The tree delivers a pointer to the one widget whose box is under it, and
/// that answer is wrong three times over: a menu is drawn in a layer no box
/// covers, a gesture that has left its box is still the gesture the hand is
/// making, and an item pulled out of a list has to be heard both by the list
/// it came from and by whatever it is dropped on. So the routers the document
/// mounted are asked first, and the tree hears the event only when none of
/// them took it.
impl<Action> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    fn bank_pointer(&self, event: &PointerEvent) {
        let PointerEvent::Down(button) = event else {
            return;
        };
        if self
            .popovers
            .iter()
            .any(|popover| popover.state.standing().is_some())
        {
            return;
        }
        let position = button.state.logical_position();
        let point = Point::new(position.x, position.y);
        if self.window_owns(point) {
            return;
        }
        for popover in &self.popovers {
            popover.state.bank(point);
        }
    }

    fn dismisses_popover(&mut self, event: &PointerEvent) -> Result<bool, MasonryRootError> {
        let PointerEvent::Down(button) = event else {
            return Ok(false);
        };
        let Some(popover) = self
            .popovers
            .iter()
            .rev()
            .find(|popover| popover.state.standing().is_some())
        else {
            return Ok(false);
        };
        let position = button.state.logical_position();
        let point = Point::new(position.x, position.y);
        if self.window_owns(point) || popover.state.surface().contains(point) {
            return Ok(false);
        }
        let action = (popover.dismiss)();
        self.push_action(Box::new(action))?;
        Ok(true)
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
        let menu = self.engines.iter().any(|engine| engine.has_open_picker());
        if !menu && self.dismisses_popover(&event)? {
            return Ok(Handled::Yes);
        }
        self.bank_pointer(&event);
        let at = picker::at(&event);
        let handled = if self.route_engines(&event)? {
            self.sync()?;
            Handled::Yes
        } else {
            self.route_root_pointer(event)?
        };
        if let Some(point) = at {
            self.show_cursor(point);
        }
        Ok(handled)
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
            if let Some(window) = &self.window {
                window.pointer.set(Some(point));
                if window.carrying
                    && matches!(event, PointerEvent::Move(_))
                    && let Some(layer) = window.layer
                {
                    self.root.edit_widget(layer, |mut layer| {
                        layer.ctx.request_paint_only();
                    });
                }
            }
        }
    }

    /// Asks every router the document mounted, and says whether one took the
    /// event outright.
    ///
    /// A router with nothing under the hand answers nothing, so the order only
    /// decides who hears a point two of them could claim, and the one drawn on
    /// top hears it.
    ///
    /// A router that holds the pointer, or takes it here, ends the walk — and
    /// the event that lets go is still its own, which is what makes a double
    /// click that ends a drag land on the control that was dragged. A router
    /// that merely answers — a list that hears the item it is carrying move —
    /// leaves the event for the routers below it and for the tree, which is
    /// how the module the item is dropped on learns the hand is above it.
    fn route_engines(&mut self, event: &PointerEvent) -> Result<bool, MasonryRootError> {
        let Some((input, at)) = picker::pointing(event, &mut self.double_click, self.scale) else {
            return Ok(false);
        };
        let covered = self.covering_surface(at);
        for engine in self.routers() {
            let owner = engine.owner();
            let held = engine.captures_pointer();
            if !held && covered.is_some_and(|index| !self.popovers[index].controls.contains(&owner))
            {
                continue;
            }
            let routed = engine.route(input, at);
            for node in &routed.repaint {
                self.root.edit_widget(*node, |mut widget| {
                    widget.ctx.request_paint_only();
                });
            }
            if matches!(event, PointerEvent::Down(_)) {
                self.sync_picker_focus(owner, routed.focused);
            }
            let captured = routed.outcome.is_captured();
            if let Some(action) = routed.outcome.value() {
                self.push_action(Box::new(action))?;
            }
            if captured || held {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The open surface this point lands in, when one covers it.
    ///
    /// A surface floats above the document, so the room it covers is its own.
    /// The topmost one wins, the same order a press outside them is dismissed
    /// in, and an engine that already holds the pointer keeps it either way.
    fn covering_surface(&self, at: Option<Pt>) -> Option<usize> {
        let at = at?;
        let point = Point::new(at.x.into(), at.y.into());
        self.popovers.iter().rposition(|popover| {
            popover.state.standing().is_some() && popover.state.surface().contains(point)
        })
    }

    fn route_root_pointer(&mut self, event: PointerEvent) -> Result<Handled, MasonryRootError> {
        let handled = self.root.handle_pointer_event(event);
        self.sync_popovers();
        self.sync()?;
        Ok(handled)
    }

    /// The routers in the order they are asked: from the top of the document
    /// down, which is the order they were stacked in, reversed.
    fn routers(&self) -> Vec<Rc<HostedEngine>> {
        self.engines.iter().rev().map(Rc::clone).collect()
    }

    /// Says what the hand is doing, from the router that owns it.
    ///
    /// The tree asks whatever sits under the pointer, and by the time a track
    /// is over the deck that is the deck, which knows nothing about the drag.
    /// The list that started it does, so its answer is the last word.
    fn show_cursor(&mut self, point: Pt) {
        let shape = self
            .routers()
            .into_iter()
            .map(|engine| engine.cursor(point))
            .find(|shape| *shape != CursorShape::None);
        if let Some(shape) = shape {
            self.platform
                .push(RenderRootSignal::SetCursor(cursor_icon(shape)));
        }
    }

    fn sync_picker_focus(&mut self, owner: WidgetId, focused: bool) {
        if focused {
            self.root.focus_on(Some(owner));
        } else if self.root.focused_widget() == Some(owner) {
            self.root.focus_on(None);
        }
    }

    fn window_answers_first(&self, event: &PointerEvent) -> bool {
        let PointerEvent::Down(button) = event else {
            return false;
        };
        let position = button.state.logical_position();
        self.window_owns(Point::new(position.x, position.y))
    }

    fn window_owns(&self, point: Point) -> bool {
        let Some(layer) = self.window.as_ref().and_then(|window| window.layer) else {
            return false;
        };
        self.root
            .get_widget(layer)
            .and_then(|layer| layer.find_widget_under_pointer(point))
            .is_some()
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
    text: TextContext,
    resize_edges: bool,
    resize_edge: f32,
}

impl WindowLayer {
    pub(crate) fn new(
        ghost: Option<DragGhost>,
        resize_edges: bool,
        pointer: Rc<Cell<Option<Pt>>>,
        map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
        skin: &Skin,
    ) -> Self {
        Self {
            ghost,
            map_event,
            pointer,
            resize_edges,
            active: None,
            resize_edge: skin.window.resize_edge,
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

    /// Takes up what the pointer is carrying now, and says whether that changed
    /// what this layer draws.
    fn carry(&mut self, label: Option<&str>) -> bool {
        self.ghost.as_mut().is_some_and(|ghost| ghost.carry(label))
    }

    fn command_at(&self, size: Size, pointer: Option<Pt>) -> Option<WindowCommand> {
        self.resize_layer(size)
            .and_then(|layer| layer.action_at(pointer).copied())
    }

    fn resize_layer(&self, size: Size) -> Option<crate::render::HostLayer<WindowCommand>> {
        self.resize_edges
            .then(|| WindowSurface::frame(Self::bounds(size), self.resize_edge))
    }
}

impl Widget for WindowLayer {
    type Action = HostAction;

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        _node: &mut AccessNode,
    ) {
    }

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
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

    fn layout(
        &mut self,
        _ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        constraints: &BoxConstraints,
    ) -> Size {
        constraints.max()
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("KitharaWindowLayer", id = id.trace())
    }

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

    fn paint(&mut self, ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, scene: &mut Scene) {
        let Some(ghost) = &self.ghost else {
            return;
        };
        let layer = ghost.layer(self.pointer.get(), Self::bounds(ctx.size()), &mut self.text);
        replay(layer.draw(), &mut VelloBackend::new(scene));
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}
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
