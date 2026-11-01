use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use kithara_test_macros as kithara;
use masonry::{
    accesskit::TreeUpdate,
    app::{RenderRoot, RenderRootOptions, RenderRootSignal},
    core::{
        CursorIcon, ErasedAction, Handled, NewWidget, PointerEvent, TextEvent, Widget, WidgetId,
        WindowEvent,
    },
    kurbo::{Point, Rect as MasonryRect},
    ui_events::keyboard::{Key, NamedKey},
    vello::Scene,
};
use thiserror::Error;

use super::{
    built::{
        BlockRegistration, MasonryNode, NodeBox, PopoverRegistration, Registrations, RootParts,
        Watched, WindowTracker, Within,
    },
    custom::HostAction,
    node::Node,
    picker::{self, HostedEngine},
    popover::SurfaceKind,
    window_layer::WindowLayer,
};
#[cfg(feature = "capture")]
use crate::draw::Rgba;
use crate::{
    draw::Pt,
    hosts::drag::DragSession,
    interact::{
        CursorShape,
        masonry::{changes_modifiers, cursor_icon},
    },
    masonry::{
        external::ExternalImageDeclaration, poses::placements, shader::ShaderDeclaration,
        vis::VisDeclaration,
    },
    render::{Published, document::Ctx},
};

/// A Masonry render root with native-layer synchronization and typed actions.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct MasonryRoot<Action> {
    window: Option<WindowTracker>,
    signals: Rc<RefCell<VecDeque<RenderRootSignal>>>,
    #[field(get, vis = "pub")]
    root: RenderRoot,
    actions: Vec<Action>,
    blocks: Vec<Within<BlockRegistration>>,
    boxes: Vec<NodeBox>,
    engines: Vec<Within<Rc<HostedEngine>>>,
    native: Vec<WidgetId>,
    platform: Vec<RenderRootSignal>,
    popovers: Vec<Within<PopoverRegistration>>,
    watched: Vec<Within<Watched>>,
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
    drag: DragSession,
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
        let (base, mut layers, registrations, boxes, native, window) = RootParts::from(node);
        let Registrations {
            watched,
            blocks,
            mut popovers,
            engines,
            ..
        } = registrations;
        stack_modals_on_top(&mut layers, &mut popovers, window.as_ref());
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
            drag: DragSession::default(),
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
        let kept = !matches!(event, TextEvent::WindowFocusChange(_))
            && !changes_modifiers(&event)
            && self.modal_keeps_keys();
        let handled = if kept {
            Handled::No
        } else {
            let handled = self.root.handle_text_event(event);
            self.sync()?;
            handled
        };
        if handled == Handled::No
            && dismiss
            && let Some(action) = self
                .popovers
                .iter()
                .rev()
                .find(|popover| popover.item.state.standing().is_some())
                .map(|popover| (popover.item.dismiss)())
        {
            self.push_action(Box::new(action))?;
            return Ok(Handled::Yes);
        }
        Ok(if kept { Handled::Yes } else { handled })
    }

    /// Whether a modal stands while the keyboard focus is outside it, so a
    /// key pressed there is the modal's.
    fn modal_keeps_keys(&self) -> bool {
        let Some(modal) = self.popovers.iter().rev().find(|popover| {
            matches!(popover.item.state.kind(), SurfaceKind::Modal)
                && popover.item.state.standing().is_some()
        }) else {
            return false;
        };
        !self.root.focused_widget().is_some_and(|focused| {
            self.root
                .get_widget(modal.item.layer)
                .is_some_and(|layer| layer.find_widget_by_id(focused).is_some())
        })
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

    fn read_widget<W: Widget, T>(
        &self,
        id: WidgetId,
        read: impl FnOnce(&W) -> Option<T>,
    ) -> Option<T> {
        read(self.root.get_widget(id)?.downcast::<W>()?.inner())
    }

    /// Whether the widget holding the keyboard takes text input, so a paste
    /// the window reads belongs to it.
    pub(crate) fn editing_text(&self) -> bool {
        self.root
            .focused_widget()
            .and_then(|id| self.read_widget(id, |node: &Node| Some(node.accepts_text_input())))
            == Some(true)
    }

    /// The device pictures the shown custom widgets drew into the scene, each
    /// once. A stashed widget's last paint is not part of the scene.
    pub(crate) fn external_images(&self) -> Vec<ExternalImageDeclaration> {
        let shown = self.native.iter().filter_map(|id| {
            let widget = self.root.get_widget(*id)?.downcast::<Node>()?;
            (!widget.ctx().is_stashed()).then(|| widget.external_images().to_vec())
        });
        let mut images: Vec<ExternalImageDeclaration> = Vec::new();
        for image in shown.flatten() {
            if !images.contains(&image) {
                images.push(image);
            }
        }
        images
    }

    /// The colour the node `id` writes its text in right now.
    #[cfg(feature = "capture")]
    pub(crate) fn ink_of(&self, id: WidgetId) -> Option<Rgba> {
        self.read_widget(id, Node::ink)
    }

    /// Reports whether Masonry requested another paint or animation frame.
    pub(crate) fn needs_frame(&self) -> bool {
        frame_requested(&self.platform) || self.animates || self.moved
    }

    fn push_action(&mut self, action: ErasedAction) -> Result<(), MasonryRootError> {
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
            .filter_map(|engine| engine.item.take_changed_menu())
            .collect();
        for layer in changed {
            self.root.edit_widget(layer, |mut layer| {
                layer.ctx.request_paint_only();
            });
        }
    }

    fn sync_popovers(&self) {
        for popover in &self.popovers {
            popover
                .item
                .state
                .set_anchor(self.anchor_box(popover.item.anchor));
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
            .any(|popover| popover.item.state.standing().is_some())
        {
            return;
        }
        let position = button.state.logical_position();
        let point = Point::new(position.x, position.y);
        if self.window_owns(point) {
            return;
        }
        for popover in &self.popovers {
            popover.item.state.bank(point);
        }
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
            let state = &popover.item.state;
            state.standing().is_some()
                && match state.kind() {
                    SurfaceKind::Popover { .. } => state.surface().contains(point),
                    SurfaceKind::Modal => true,
                }
        })
    }

    fn dismisses_popover(&mut self, event: &PointerEvent) -> Result<bool, MasonryRootError> {
        let PointerEvent::Down(button) = event else {
            return Ok(false);
        };
        let Some(popover) = self
            .popovers
            .iter()
            .rev()
            .find(|popover| popover.item.state.standing().is_some())
        else {
            return Ok(false);
        };
        if matches!(popover.item.state.kind(), SurfaceKind::Modal) {
            return Ok(false);
        }
        let position = button.state.logical_position();
        let point = Point::new(position.x, position.y);
        if self.window_owns(point) || popover.item.state.surface().contains(point) {
            return Ok(false);
        }
        let action = (popover.item.dismiss)();
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
        let menu = self
            .engines
            .iter()
            .any(|engine| engine.item.has_open_picker());
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
        if let Some(point) = picker::at(event)
            && let Some(window) = &self.window
        {
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

    /// Routes topmost-first and reports whether a router took the event outright.
    /// An existing or newly taken pointer capture ends the walk, including its release
    /// event, so a double click ending a drag reaches the dragged control. A router
    /// that only observes leaves the event for lower routers and the tree, allowing
    /// the destination module to observe a carried item above it.
    fn route_engines(&mut self, event: &PointerEvent) -> Result<bool, MasonryRootError> {
        let Some((input, at)) = picker::pointing(event, &mut self.double_click, self.scale) else {
            return Ok(false);
        };
        let covered = self.covering_surface(at);
        for engine in self.routers() {
            let owner = engine.owner();
            let held = engine.captures_pointer();
            if !self.reaches(&engine, covered) {
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
            if let Some(event) = routed.drag {
                self.follow_drag(&engine, &event)?;
            }
            if captured || held {
                if matches!(event, PointerEvent::Down(_)) {
                    self.leave_focus(owner);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A press an engine took never reaches the tree, where a press elsewhere
    /// ends the keyboard focus, so it ends here.
    fn leave_focus(&mut self, owner: WidgetId) {
        if self
            .root
            .focused_widget()
            .is_some_and(|focused| focused != owner)
        {
            self.root.focus_on(None);
        }
    }

    /// Whether the pointer reaches this engine past the surface that covers
    /// the point, if one does: an engine holding the pointer keeps it, and
    /// otherwise only the controls the surface answers for are reached.
    fn reaches(&self, engine: &HostedEngine, covered: Option<usize>) -> bool {
        engine.captures_pointer()
            || covered
                .is_none_or(|index| self.popovers[index].item.controls.contains(&engine.owner()))
    }

    fn follow_drag(
        &mut self,
        engine: &HostedEngine,
        event: &Published,
    ) -> Result<(), MasonryRootError> {
        if !self.window.as_ref().is_some_and(|window| window.drops) {
            return Ok(());
        }
        let hovered = self.drag.hovered().map(str::to_owned);
        let label = self.drag.label().map(str::to_owned);
        let engines = &self.engines;
        let dropped = self.drag.follow(event, |table, index| {
            engines
                .iter()
                .find_map(|engine| engine.item.carried(table, index))
        });
        if self.drag.hovered() != hovered.as_deref() {
            let hovered = self.drag.hovered().map(str::to_owned);
            self.light_zones(hovered.as_deref());
        }
        if self.drag.label() != label.as_deref() {
            let label = self.drag.label().map(str::to_owned);
            self.show_carried(label.as_deref());
        }
        if let Some(dropped) = dropped {
            self.push_action(Box::new(engine.action(dropped)))?;
        }
        Ok(())
    }

    fn light_zones(&mut self, hovered: Option<&str>) {
        for watched in &self.watched {
            let Watched::Zone { id, path } = &watched.item else {
                continue;
            };
            let on = hovered == Some(path.as_str());
            self.root.edit_widget(*id, |mut widget| {
                let mut node = widget.downcast::<Node>();
                if node.widget.light(on) {
                    node.ctx.request_paint_only();
                    node.ctx.request_post_paint();
                }
            });
        }
    }

    fn show_carried(&mut self, label: Option<&str>) {
        let Some(window) = &mut self.window else {
            return;
        };
        let Some(layer) = window.layer else {
            return;
        };
        window.carrying = label.is_some();
        self.root.edit_widget(layer, |mut widget| {
            let mut window = widget.downcast::<WindowLayer>();
            if window.widget.carry(label) {
                window.ctx.request_paint_only();
            }
        });
    }

    fn route_root_pointer(&mut self, event: PointerEvent) -> Result<Handled, MasonryRootError> {
        let handled = self.root.handle_pointer_event(event);
        self.sync_popovers();
        self.sync()?;
        Ok(handled)
    }

    fn routers(&self) -> Vec<Rc<HostedEngine>> {
        self.engines
            .iter()
            .rev()
            .filter(|engine| engine.within.shown())
            .map(|engine| Rc::clone(&engine.item))
            .collect()
    }

    /// Says what the hand is doing, from the router that owns it.
    ///
    /// The tree asks whatever sits under the pointer, and by the time a track
    /// is over the deck that is the deck, which knows nothing about the drag.
    /// The list that started it does, so its answer is the last word.
    fn show_cursor(&mut self, point: Pt) {
        let covered = self.covering_surface(Some(point));
        let shape = self
            .routers()
            .into_iter()
            .filter(|engine| self.reaches(engine, covered))
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

impl<Action> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// Shows what the pointer is carrying now.
    ///
    /// The ghost is a value the window layer draws, not shape the layer was
    /// mounted with: the layer stands for the life of the window, and what the
    /// pointer carries changes under it.
    fn carry_ghost(&mut self) -> bool {
        let Some(window) = &mut self.window else {
            return false;
        };
        let Some(layer) = window.layer else {
            return false;
        };
        let label = self.drag.label();
        window.carrying = label.is_some();
        self.root.edit_widget(layer, |mut widget| {
            let mut window = widget.downcast::<WindowLayer>();
            let carried = window.widget.carry(label);
            if carried {
                window.ctx.request_paint_only();
            }
            carried
        })
    }

    fn open_surfaces(&mut self, ctx: Ctx<'_, '_>) {
        let changed: Vec<WidgetId> = self
            .popovers
            .iter()
            .filter(|popover| {
                let open = popover.within.shown() && ctx.flag(Some(&popover.item.flag));
                open != popover.item.state.is_open()
            })
            .map(|popover| {
                popover.item.state.latch(!popover.item.state.is_open());
                popover.item.layer
            })
            .collect();
        for layer in changed {
            self.root.edit_widget(layer, |mut layer| {
                layer.ctx.request_layout();
            });
        }
    }

    /// Walks the document again for the poses alone, and moves whatever the
    /// walk now puts somewhere else.
    ///
    /// Nothing is watched this way unless the document declares an object an
    /// endpoint drives, so a page that never moves pays for none of this.
    fn place_objects(&mut self, ctx: Ctx<'_, '_>) -> bool {
        if !ctx.ui.driven {
            return false;
        }
        let placed = placements(&ctx.ui.root, ctx);
        let mut moved = false;
        for watched in &self.watched {
            let Watched::Placed { id, path } = &watched.item else {
                continue;
            };
            let Some(transform) = placed.get(path).copied() else {
                continue;
            };
            let placed = self.root.edit_widget(*id, |mut widget| {
                let mut node = widget.downcast::<Node>();
                let moved = node.widget.place(transform);
                if moved {
                    node.ctx.request_paint_only();
                }
                moved
            });
            if placed {
                kithara::probe_event!(masonry_object_moved, widget = id.to_raw());
            }
            moved |= placed;
        }
        moved
    }

    /// Refreshes mounted values and poses without rebuilding, preserving in-flight
    /// gestures and pointer capture while updating every control sharing an endpoint.
    /// Values are re-read per control; surrounding objects determine poses through
    /// one document walk. Blocks stand first so hidden content remains unread.
    pub fn refresh(&mut self, ctx: Ctx<'_, '_>) {
        self.stand_blocks(ctx);
        let shown = self.show_values(ctx);
        self.reread_plans(ctx);
        let placed = self.place_objects(ctx);
        self.open_surfaces(ctx);
        let carried = self.carry_ghost();
        self.moved = shown || placed || carried;
    }

    /// Carries the frame just read into the gestures already mounted.
    ///
    /// A control answers a hand against what it is showing, and what it is
    /// showing changes without the tree changing shape. The immediate host
    /// resolves that afresh every frame because it rebuilds; this one re-reads
    /// it in place.
    fn reread_plans(&mut self, ctx: Ctx<'_, '_>) {
        for engine in self.engines.iter().filter(|engine| engine.within.shown()) {
            engine.item.reread(ctx);
        }
    }

    fn show_values(&mut self, ctx: Ctx<'_, '_>) -> bool {
        let mut moved = false;
        for watched in self.watched.iter().filter(|watched| watched.within.shown()) {
            match &watched.item {
                Watched::Read { id, binding } => {
                    let Some(value) = ctx.read(binding) else {
                        continue;
                    };
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let before = node.widget.text_size();
                        let shown = node.widget.show_live(&value);
                        if shown && node.widget.text_size() != before {
                            node.ctx.request_layout();
                        } else if shown {
                            node.ctx.request_paint_only();
                        }
                        shown
                    });
                }
                Watched::Snapshot { id } => {
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let shown = node.widget.refresh(ctx);
                        if shown {
                            node.ctx.request_paint_only();
                        }
                        shown
                    });
                }
                Watched::Spot { id, binding } => {
                    let Some(at) = ctx.point(Some(binding)) else {
                        continue;
                    };
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let moved = node.widget.move_spot(at);
                        if moved {
                            node.ctx.request_layout();
                        }
                        moved
                    });
                }
                Watched::Lit { id, flag } => {
                    let on = ctx.flag(Some(flag));
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let lit = node.widget.light(on);
                        if lit {
                            node.ctx.request_paint_only();
                        }
                        lit
                    });
                }
                Watched::Placed { .. } | Watched::Zone { .. } => {}
            }
        }
        moved
    }

    fn stand_blocks(&mut self, ctx: Ctx<'_, '_>) {
        let mut changed: Vec<WidgetId> = Vec::new();
        for block in self.blocks.iter().rev() {
            if block.within.shown() && block.item.state.latch(ctx.flag(Some(&block.item.hidden))) {
                changed.push(block.item.flow);
            }
        }
        if changed.is_empty() {
            return;
        }
        // Editing a flow immediately runs composition, which reads its controls.
        // Publish visibility to their engines before exposing the new layout.
        self.stand_engines();
        for flow in changed {
            self.root.edit_widget(flow, |mut flow| {
                flow.ctx.request_layout();
            });
        }
    }

    fn stand_engines(&mut self) {
        for engine in &self.engines {
            engine.item.stand(engine.within.shown());
        }
        self.sync_menus();
    }
}

/// Stacks every modal above the other surfaces and layers, under only the
/// window's own layer, whatever its place in the document: the layer order is
/// the paint order, and the surface order is the order input finds them in.
fn stack_modals_on_top(
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
