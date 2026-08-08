use std::{cell::Cell, rc::Rc};

use masonry::{
    accesskit::{Node as AccessNode, Role},
    core::{
        AccessCtx, BoxConstraints, ChildrenIds, ComposeCtx, EventCtx, LayoutCtx, NewWidget,
        PaintCtx, PointerEvent, PropertiesMut, PropertiesRef, QueryCtx, RegisterCtx, Widget,
        WidgetId, WidgetPod, WidgetRef, find_widget_under_pointer,
    },
    kurbo::{Point, Rect as MasonryRect, Size as MasonrySize, Vec2},
    vello::Scene,
};
use num_traits::cast::AsPrimitive;
use tracing::{Span, trace_span};

use super::{custom::HostAction, flex::box_constraints, node::Node};
use crate::{
    backends::VelloBackend,
    draw::{DrawListBuilder, Rect, Rgba, replay},
    module::{PopoverAlign, PopoverAt},
    render::{Skin, place_popover},
    solve,
};

#[derive(Default)]
pub(crate) struct PopoverState {
    anchor: Cell<MasonryRect>,
    open: Cell<bool>,
    pointer: Cell<Option<Point>>,
    press: Cell<Option<Point>>,
    surface: Cell<MasonryRect>,
}

impl PopoverState {
    pub(crate) fn latch(&self, open: bool) {
        let was_open = self.open.replace(open);
        if open && !was_open {
            self.pointer.set(self.press.take());
        }
    }

    pub(crate) fn bank(&self, point: Point) {
        self.press.set(Some(point));
    }

    pub(crate) fn set_anchor(&self, anchor: MasonryRect) {
        self.anchor.set(anchor);
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.get()
    }

    pub(crate) fn surface(&self) -> MasonryRect {
        self.surface.get()
    }
}

pub(crate) struct PopoverLayer {
    align: PopoverAlign,
    at: PopoverAt,
    background: Rgba,
    border: Rgba,
    border_radius: f32,
    border_width: f32,
    cap: Rgba,
    cap_height: f32,
    child: WidgetPod<Node>,
    declared: solve::Size<solve::Length>,
    state: Rc<PopoverState>,
    surface_size: MasonrySize,
}

impl PopoverLayer {
    pub(crate) fn new(
        content: NewWidget<Node>,
        declared: solve::Size<solve::Length>,
        state: Rc<PopoverState>,
        at: PopoverAt,
        align: PopoverAlign,
        skin: &Skin,
    ) -> Self {
        let frame = skin.pop.frame;
        Self {
            align,
            at,
            background: skin.rgba(skin.pop.background),
            border: skin.rgba(frame.border),
            border_radius: frame.radius,
            border_width: frame.border_width,
            cap: skin.rgba(skin.pop.cap_color),
            cap_height: skin.pop.cap_height,
            child: content.to_pod(),
            declared,
            state,
            surface_size: MasonrySize::ZERO,
        }
    }
}

impl Widget for PopoverLayer {
    type Action = HostAction;

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if matches!(event, PointerEvent::Down(_)) {
            ctx.set_handled();
        }
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.child);
    }

    fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        constraints: &BoxConstraints,
    ) -> MasonrySize {
        let frame = f64::from(self.border_width);
        let cap = f64::from(self.cap_height);
        let viewport = constraints.max();
        let inner_max = solve::Size::new(
            (viewport.width - frame * 2.0).max(0.0).as_(),
            (viewport.height - frame * 2.0 - cap).max(0.0).as_(),
        );
        let limits = solve::Limits::new(solve::Size::ZERO, inner_max);
        Node::set_child_limits(ctx, &mut self.child, limits);
        let measured = ctx.run_layout(&mut self.child, &box_constraints(limits));
        let content = limits.resolve(
            self.declared.width,
            self.declared.height,
            solve::Size::new(measured.width.as_(), measured.height.as_()),
        );
        let exact = solve::Limits::new(content, content);
        Node::set_child_limits(ctx, &mut self.child, exact);
        ctx.run_layout(
            &mut self.child,
            &BoxConstraints::tight(MasonrySize::new(
                f64::from(content.width),
                f64::from(content.height),
            )),
        );
        ctx.place_child(&mut self.child, Point::new(frame, frame + cap));
        self.surface_size = MasonrySize::new(
            f64::from(content.width) + frame * 2.0,
            f64::from(content.height) + frame * 2.0 + cap,
        );
        viewport
    }

    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        let position = place(
            self.state.anchor.get(),
            (self.at == PopoverAt::Pointer)
                .then(|| self.state.pointer.get())
                .flatten(),
            self.surface_size,
            ctx.size(),
            self.align,
        );
        self.state
            .surface
            .set(MasonryRect::from_origin_size(position, self.surface_size));
        ctx.set_animated_child_scroll_translation(
            &mut self.child,
            Vec2::new(position.x, position.y),
        );
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, scene: &mut Scene) {
        let surface = self.state.surface();
        let rect = Rect {
            x: surface.x0.as_(),
            y: surface.y0.as_(),
            w: surface.width().as_(),
            h: surface.height().as_(),
        };
        let mut list = DrawListBuilder::default();
        list.fill_rounded_rect(rect, self.border_radius, self.background);
        list.fill_rect(
            Rect {
                h: self.cap_height,
                w: (rect.w - self.border_width * 2.0).max(0.0),
                x: rect.x + self.border_width,
                y: rect.y + self.border_width,
            },
            self.cap,
        );
        list.stroke_rounded_rect(rect, self.border_radius, self.border, self.border_width);
        replay(&list.finish(), &mut VelloBackend::new(scene));
    }

    fn accessibility_role(&self) -> Role {
        Role::Dialog
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        _node: &mut AccessNode,
    ) {
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[self.child.id()])
    }

    fn accepts_pointer_interaction(&self) -> bool {
        true
    }

    fn find_widget_under_pointer<'ctx>(
        &'ctx self,
        ctx: QueryCtx<'ctx>,
        pos: Point,
    ) -> Option<WidgetRef<'ctx, dyn Widget>> {
        if !self.state.surface().contains(pos) {
            return None;
        }
        find_widget_under_pointer(self, ctx, pos)
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("KitharaPopoverLayer", id = id.trace())
    }
}

fn place(
    anchor: MasonryRect,
    pointer: Option<Point>,
    surface: MasonrySize,
    viewport: MasonrySize,
    align: PopoverAlign,
) -> Point {
    let point = place_popover(
        Rect {
            x: anchor.x0.as_(),
            y: anchor.y0.as_(),
            w: anchor.width().as_(),
            h: anchor.height().as_(),
        },
        pointer.map(|point| crate::draw::Pt {
            x: point.x.as_(),
            y: point.y.as_(),
        }),
        solve::Size::new(surface.width.as_(), surface.height.as_()),
        solve::Size::new(viewport.width.as_(), viewport.height.as_()),
        align,
    );
    Point::new(f64::from(point.x), f64::from(point.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_matches_iced_flip_alignment_and_clamp_contract() {
        let anchor = MasonryRect::new(40.0, 30.0, 90.0, 50.0);
        assert_eq!(
            place(
                anchor,
                None,
                MasonrySize::new(100.0, 60.0),
                MasonrySize::new(200.0, 140.0),
                PopoverAlign::Start,
            ),
            Point::new(39.0, 50.0),
        );
        assert_eq!(
            place(
                MasonryRect::new(180.0, 120.0, 195.0, 135.0),
                None,
                MasonrySize::new(100.0, 60.0),
                MasonrySize::new(200.0, 140.0),
                PopoverAlign::End,
            ),
            Point::new(96.0, 60.0),
        );
    }

    #[test]
    fn every_open_latches_its_own_press_after_a_close() {
        let state = PopoverState::default();
        let first = Point::new(12.0, 18.0);
        let second = Point::new(44.0, 52.0);

        state.bank(first);
        state.latch(true);
        assert!(state.is_open());
        assert_eq!(state.pointer.get(), Some(first));

        state.latch(false);
        assert!(!state.is_open());

        state.bank(second);
        state.latch(true);
        assert!(state.is_open());
        assert_eq!(state.pointer.get(), Some(second));
    }
}
