use std::{cell::Cell, rc::Rc};

use kithara_platform::time::Duration;
use masonry::{
    accesskit::{Node as AccessNode, Role},
    core::{
        AccessCtx, BoxConstraints, ChildrenIds, CursorIcon, EventCtx, LayoutCtx, PaintCtx,
        PointerEvent, PropertiesMut, PropertiesRef, QueryCtx, RegisterCtx, UpdateCtx, Widget,
        WidgetId, WidgetRef, find_widget_under_pointer,
    },
    kurbo::{Affine, Point, Rect as MasonryRect, Size as MasonrySize},
    vello::Scene,
};
use num_traits::cast::AsPrimitive;
use tracing::{Span, trace_span};

use super::{
    MasonryControl, Repaint, Size2, SizeLimits, TextMeasurer,
    custom::{HostAction, MountedCustom},
    node::pointer_button,
};
use crate::{
    backends::VelloBackend,
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba, Transform, replay},
    interact::{
        CursorShape, Hit, Input, MOUSE, Outcome, PointerInput, PointerOwnership, PointerPhase,
    },
    render::{HostLayer, ReadValue, UiEvent, WindowCommand, WindowLayerProgram},
    skin::TextRoleSkin,
    text::{TextContext, TextResources},
};

pub(crate) enum Leaf {
    Empty,
    Control(Box<dyn MasonryControl>),
    Text {
        content: String,
        role: TextRoleSkin,
        padding_x: f32,
        color: Rgba,
        text: Box<TextContext>,
    },
    Custom {
        widget: Box<dyn MountedCustom>,
        text: Box<TextContext>,
    },
}

impl Leaf {
    pub(crate) fn measure(&mut self, limits: crate::solve::Limits) -> crate::solve::Size {
        match self {
            Self::Empty => crate::solve::Size::ZERO,
            Self::Control(control) => control.measure(),
            Self::Text {
                content,
                role,
                padding_x,
                text,
                ..
            } => {
                let run = text.shape(content, *role, wrap_width(limits.max().width, *padding_x));
                crate::solve::Size::new(run.width() + *padding_x * 2.0, run.height())
            }
            Self::Custom { widget, text } => {
                let mut text = TextMeasurer::new(text);
                let size = widget.measure(
                    &mut text,
                    SizeLimits::new(
                        Size2::new(limits.min().width, limits.min().height),
                        Size2::new(limits.max().width, limits.max().height),
                    ),
                );
                crate::solve::Size::new(size.w, size.h)
            }
        }
    }

    pub(crate) fn paint(&mut self, bounds: Rect, scene: &mut Scene) {
        if let Self::Control(control) = self {
            replay(&control.draw_list(bounds), &mut VelloBackend::new(scene));
            return;
        }
        let mut list = DrawListBuilder::default();
        match self {
            Self::Empty | Self::Control(_) => {}
            Self::Text {
                content,
                role,
                padding_x,
                color,
                text,
            } => {
                if !content.is_empty() {
                    let max_width = (bounds.w - *padding_x * 2.0).max(0.0);
                    let run = text.shape(content, *role, Some(max_width));
                    list.text(
                        &run,
                        content,
                        Transform::translate(Pt {
                            x: *padding_x,
                            y: (bounds.h - run.height()) / 2.0,
                        }),
                        *color,
                    );
                }
            }
            Self::Custom { widget, text } => {
                widget.paint(&mut list, &mut TextMeasurer::new(text), bounds);
            }
        }
        replay(&list.finish(), &mut VelloBackend::new(scene));
    }

    pub(crate) fn added(&self, ctx: &mut UpdateCtx<'_>) {
        if matches!(self.repaint(), Repaint::NextFrame | Repaint::Continuous) {
            ctx.request_anim_frame();
        }
    }

    pub(crate) fn animate(&self, ctx: &mut UpdateCtx<'_>) {
        let repaint = self.repaint();
        ctx.request_paint_only();
        if repaint == Repaint::Continuous {
            ctx.request_anim_frame();
        }
    }

    pub(crate) fn input(&mut self, input: Input<'_>, hit: Hit) -> Outcome<HostAction> {
        match self {
            Self::Control(control) => control.input(input, &hit),
            Self::Custom { widget, .. } => widget.input(input, hit),
            Self::Empty | Self::Text { .. } => Outcome::IGNORED,
        }
    }

    pub(crate) fn frame(&mut self, elapsed: Duration) -> Option<HostAction> {
        match self {
            Self::Custom { widget, .. } => widget.frame(elapsed),
            Self::Empty | Self::Control(_) | Self::Text { .. } => None,
        }
    }

    pub(crate) fn repaint(&self) -> Repaint {
        if let Self::Control(control) = self {
            return control.repaint();
        }
        let Self::Custom { widget, .. } = self else {
            return Repaint::default();
        };
        widget.repaint()
    }

    pub(crate) fn hover(&mut self, hovered: bool) -> bool {
        match self {
            Self::Control(control) => control.hover(hovered),
            Self::Custom { .. } | Self::Empty | Self::Text { .. } => false,
        }
    }

    /// Takes what this leaf's endpoint now reads. This is the one way a mounted
    /// leaf learns a new value without the tree being rebuilt around it.
    pub(crate) fn set_read(&mut self, value: &ReadValue<'_>) -> bool {
        match self {
            Self::Control(control) => control.set_read(value),
            Self::Text { content, .. } => match value {
                ReadValue::Text(text) => {
                    *text != content && {
                        *content = (*text).to_owned();
                        true
                    }
                }
                _ => false,
            },
            Self::Custom { .. } | Self::Empty => false,
        }
    }

    pub(crate) fn accepts_input(&self) -> bool {
        if let Self::Control(control) = self {
            return control.accepts_input();
        }
        matches!(self, Self::Custom { .. })
    }

    pub(crate) fn accepts_text_input(&self) -> bool {
        matches!(self, Self::Custom { widget, .. } if widget.accepts_text_input())
    }

    pub(crate) fn cursor(&self, hit: &Hit) -> CursorShape {
        match self {
            Self::Control(control) => control.cursor(hit),
            Self::Empty | Self::Text { .. } | Self::Custom { .. } => CursorShape::None,
        }
    }
}

fn wrap_width(width: f32, padding_x: f32) -> Option<f32> {
    width
        .is_finite()
        .then_some((width - padding_x * 2.0).max(0.0))
}

pub(crate) struct WindowLeafLayer<Program>
where
    Program: WindowLayerProgram,
{
    geometry: Rc<Cell<MasonryRect>>,
    map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    pointer: Rc<Cell<Option<Pt>>>,
    program: Program,
    state: Program::State,
}

impl<Program> WindowLeafLayer<Program>
where
    Program: WindowLayerProgram,
{
    pub(crate) fn new(
        program: Program,
        geometry: Rc<Cell<MasonryRect>>,
        pointer: Rc<Cell<Option<Pt>>>,
        map_event: Rc<dyn Fn(UiEvent) -> HostAction>,
    ) -> Self {
        Self {
            geometry,
            map_event,
            pointer,
            program,
            state: Program::State::default(),
        }
    }

    fn layer(&self, paint: bool) -> HostLayer<WindowCommand> {
        let area = self.geometry.get();
        let bounds = Rect {
            x: area.x0.as_(),
            y: area.y0.as_(),
            w: area.width().as_(),
            h: area.height().as_(),
        };
        if paint {
            self.program.layer(&self.state, bounds, self.pointer.get())
        } else {
            self.program.hit_layer(&self.state, bounds)
        }
    }

    fn input(event: &PointerEvent) -> Option<Input<'static>> {
        let (phase, button, at, clicks) = match event {
            PointerEvent::Down(event) => (
                PointerPhase::Down,
                event.button.map(pointer_button),
                Some(event.state.logical_position()),
                event.state.count,
            ),
            PointerEvent::Move(event) => (
                PointerPhase::Move,
                None,
                Some(event.current.logical_position()),
                event.current.count,
            ),
            PointerEvent::Up(event) => (
                PointerPhase::Up,
                event.button.map(pointer_button),
                Some(event.state.logical_position()),
                event.state.count,
            ),
            PointerEvent::Leave(_) => (PointerPhase::Leave, None, None, 0),
            PointerEvent::Cancel(_) => (PointerPhase::Cancel, None, None, 0),
            PointerEvent::Enter(_) | PointerEvent::Scroll(_) | PointerEvent::Gesture(_) => {
                return None;
            }
        };
        let at = at.map(|point| Pt {
            x: point.x.as_(),
            y: point.y.as_(),
        });
        Some(Input::Pointer(PointerInput::new(
            MOUSE, button, phase, at, clicks,
        )))
    }
}

impl<Program> Widget for WindowLeafLayer<Program>
where
    Program: WindowLayerProgram + 'static,
{
    type Action = HostAction;

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        let Some(input) = Self::input(event) else {
            return;
        };
        let retained = ctx.is_pointer_capture_target();
        let layer = self.layer(false);
        let (outcome, redraw) =
            self.program
                .update(&mut self.state, input, &layer, self.pointer.get());
        if redraw {
            ctx.request_paint_only();
        }
        match outcome.ownership() {
            PointerOwnership::Claim if matches!(event, PointerEvent::Down(_)) => {
                ctx.capture_pointer();
            }
            PointerOwnership::Release if ctx.is_pointer_capture_target() => ctx.release_pointer(),
            PointerOwnership::Unchanged | PointerOwnership::Claim | PointerOwnership::Release => {}
        }
        if let Some(command) = outcome.value() {
            ctx.submit_action::<HostAction>((self.map_event)(UiEvent::Window(command)));
        }
        if outcome.is_captured() || retained {
            ctx.set_handled();
        }
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn layout(
        &mut self,
        _ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        constraints: &BoxConstraints,
    ) -> MasonrySize {
        constraints.max()
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, scene: &mut Scene) {
        let layer = self.layer(true);
        if layer.draw().commands().is_empty() {
            return;
        }
        let mut local = Scene::new();
        replay(layer.draw(), &mut VelloBackend::new(&mut local));
        let bounds = layer.bounds();
        scene.append(
            &local,
            Some(Affine::translate((
                f64::from(bounds.x),
                f64::from(bounds.y),
            ))),
        );
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

    fn get_cursor(&self, _ctx: &QueryCtx<'_>, _pos: Point) -> CursorIcon {
        cursor_icon(self.layer(false).cursor_at(self.pointer.get()))
    }

    fn find_widget_under_pointer<'ctx>(
        &'ctx self,
        ctx: QueryCtx<'ctx>,
        pos: Point,
    ) -> Option<WidgetRef<'ctx, dyn Widget>> {
        self.layer(false)
            .action_at(self.pointer.get())
            .and_then(|_| find_widget_under_pointer(self, ctx, pos))
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("KitharaWindowLeafLayer", id = id.trace())
    }
}

pub(crate) struct DragProgram;

impl WindowLayerProgram for DragProgram {
    type State = ();

    fn size(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Fill, iced::Length::Fill)
    }

    fn layer(&self, _state: &(), bounds: Rect, _pointer: Option<Pt>) -> HostLayer<WindowCommand> {
        HostLayer::new(
            bounds,
            DrawList::default(),
            vec![crate::render::LayerHit::new(
                bounds,
                CursorShape::None,
                WindowCommand::Drag,
            )],
        )
    }

    fn resources(&self) -> Option<&TextResources> {
        None
    }
}

pub(super) const fn cursor_icon(shape: CursorShape) -> CursorIcon {
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::Leaf;
    use crate::{
        atoms::{painter::Labelled, tab::TabLarge, toggle::Binary},
        builtin,
        render::masonry::Painted,
        solve::{Limits, Size},
    };

    /// The retained host has to be able to give a control the width it measures
    /// for itself. Only a tab needs it today — every other mounted control is
    /// declared `Fixed`, `Fill` or a share, and an intrinsic is consulted only
    /// under `Shrink` — so a leaf that answered zero would look right until the
    /// first tab reached this host.
    #[kithara::test]
    fn a_mounted_tab_measures_its_own_word() {
        let skin = builtin::skin();
        let mut leaf = Leaf::Control(Box::new(Painted::new(
            TabLarge::new(skin),
            Labelled {
                active: true,
                label: "DECK MICRO".to_owned(),
            },
            skin,
        )));

        let measured = leaf.measure(Limits::new(Size::ZERO, Size::new(320.0, 80.0)));

        assert!(
            (measured.width - 94.0).abs() < 0.001,
            "iced gave this label and skin 94 px; the retained host measured {}",
            measured.width
        );
        assert_eq!(measured.height, skin.tab_large.height);
    }

    /// A painter with no opinion leaves both axes to the row, which is what the
    /// leaf answered for every control before it could ask one.
    #[kithara::test]
    fn a_painter_that_does_not_measure_leaves_the_box_to_the_row() {
        let skin = builtin::skin();
        let mut leaf = Leaf::Control(Box::new(Painted::new(Binary::toggle(skin), false, skin)));

        assert_eq!(
            leaf.measure(Limits::new(Size::ZERO, Size::new(320.0, 80.0))),
            Size::ZERO
        );
    }
}
